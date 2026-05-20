//! Owner of the serial port and the writer thread.
//!
//! The plugin's run loop hands every outgoing `Frame` to a [`FrameSink`].
//! [`SerialFrameSink`] is the production sink: it owns the `Box<dyn
//! SerialPort>`, runs a dedicated thread that drains a bounded channel of
//! frames, encodes each one with `Frame::encode`, and writes it to the port.
//!
//! The plugin thread *never* blocks on serial I/O — every `send()` is a
//! `try_send`. When the writer is wedged (slow port, OS-side ringbuffer
//! full) the queue saturates; we drop the *newest* frame and log at
//! `trace`. Latency is the priority over completeness; an input that arrives
//! 200 ms late is worse than a missed press.
//!
//! ACK bytes from the firmware are intentionally ignored. The host CLI's
//! `--no-ack` mode demonstrates this is correct for low-latency input
//! forwarding — round-tripping a single ACK byte at 921600 baud adds
//! ~30 µs *plus* the OS read latency to every frame.

use std::io::Write;
use std::sync::Mutex;
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context, Result};
use crossbeam_channel::{bounded, Sender};
use pico_keeb_protocol::binary::{Frame, MAX_ENCODED_LEN};

/// Bounded queue size between the plugin thread and the serial writer
/// thread. A `Frame::Kbd` is 9 bytes ≈ 80 µs on the wire at 921600 baud, so
/// 64 frames is ~5 ms of buffered output — plenty for normal keystroke
/// cadence, and small enough that a wedged writer doesn't accumulate
/// noticeable lag before the queue overflows and we surface the issue.
const WRITER_QUEUE_SIZE: usize = 64;

/// Sink for outgoing HID frames. Trait so the plugin's held-state machine
/// is testable against an in-memory recorder without opening a real port.
pub trait FrameSink: Send {
    /// Enqueue a frame. Must not block the plugin thread — drop and log
    /// internally if the underlying transport is wedged.
    fn send(&self, frame: Frame);
}

/// Production sink: owns the serial port and a writer thread.
pub struct SerialFrameSink {
    tx: Sender<WriterMsg>,
    /// `Mutex<Option<...>>` to make `Drop` and an explicit `shutdown()`
    /// both able to take the handle exactly once. The mutex is not on the
    /// hot path — only the cleanup path.
    writer: Mutex<Option<JoinHandle<()>>>,
}

enum WriterMsg {
    Frame(Frame),
    Shutdown,
}

impl SerialFrameSink {
    /// Open the serial port at `path` (e.g. `"/dev/ttyUSB0"`) at the given
    /// baud rate and spawn the writer thread. The 50 ms timeout applies to
    /// the *open* and *write* calls; on a healthy port writes complete in
    /// microseconds, so the timeout never fires in practice.
    pub fn open(path: &str, baud: u32) -> Result<Self> {
        let port = serialport::new(path, baud)
            .timeout(Duration::from_millis(50))
            .data_bits(serialport::DataBits::Eight)
            .parity(serialport::Parity::None)
            .stop_bits(serialport::StopBits::One)
            .flow_control(serialport::FlowControl::None)
            .open()
            .with_context(|| format!("opening serial port {} at {} baud", path, baud))?;

        let _ = port.clear(serialport::ClearBuffer::Input);

        let (tx, rx) = bounded::<WriterMsg>(WRITER_QUEUE_SIZE);

        let mut port = port;
        let writer = std::thread::Builder::new()
            .name("pico-keeb-writer".into())
            .spawn(move || {
                let mut buf = [0u8; MAX_ENCODED_LEN];
                while let Ok(msg) = rx.recv() {
                    let frame = match msg {
                        WriterMsg::Frame(f) => f,
                        WriterMsg::Shutdown => break,
                    };
                    let n = frame.encode(&mut buf);
                    if let Err(e) = port.write_all(&buf[..n]) {
                        log::warn!("[pico-keeb] serial write failed: {}", e);
                    }
                }
                // Best-effort flush — if the port is already gone this is
                // a no-op, but on clean shutdown it makes sure the final
                // `Frame::Reset` reaches the firmware.
                let _ = port.flush();
            })
            .context("spawning pico-keeb writer thread")?;

        Ok(Self {
            tx,
            writer: Mutex::new(Some(writer)),
        })
    }

    /// Signal the writer to flush + exit and join it. Idempotent.
    pub fn shutdown(&self) {
        // Best-effort: if the channel is already closed, the writer has
        // already exited and there is nothing to do.
        let _ = self.tx.send(WriterMsg::Shutdown);
        if let Some(handle) = self.writer.lock().ok().and_then(|mut g| g.take()) {
            if handle.join().is_err() {
                log::warn!("[pico-keeb] writer thread panicked during shutdown");
            }
        }
    }
}

impl FrameSink for SerialFrameSink {
    fn send(&self, frame: Frame) {
        if self.tx.try_send(WriterMsg::Frame(frame)).is_err() {
            // Channel full *or* writer thread gone. In the full case we
            // intentionally drop the newest frame — see module docs.
            log::trace!("[pico-keeb] writer queue full or closed, dropped frame");
        }
    }
}

impl Drop for SerialFrameSink {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
pub mod test_support {
    //! In-memory `FrameSink` for the held-state-machine tests in
    //! `lib.rs`. Records every frame in submission order.
    use super::FrameSink;
    use pico_keeb_protocol::binary::Frame;
    use std::sync::{Arc, Mutex};

    #[derive(Default, Clone)]
    pub struct RecordingSink {
        inner: Arc<Mutex<Vec<Frame>>>,
    }

    impl RecordingSink {
        pub fn new() -> Self {
            Self::default()
        }
        pub fn frames(&self) -> Vec<Frame> {
            self.inner.lock().unwrap().clone()
        }
    }

    impl FrameSink for RecordingSink {
        fn send(&self, frame: Frame) {
            self.inner.lock().unwrap().push(frame);
        }
    }
}
