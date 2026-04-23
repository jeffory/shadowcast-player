use crossbeam_channel::select;
use shadowcast_core::{Plugin, PluginContext};

pub struct LoggerPlugin;

impl Plugin for LoggerPlugin {
    fn name(&self) -> &str {
        "example-logger"
    }

    fn run(&mut self, ctx: PluginContext) {
        let mut frame_count: u64 = 0;
        loop {
            select! {
                recv(ctx.frame_rx) -> msg => {
                    match msg {
                        Ok(frame) => {
                            frame_count += 1;
                            if frame_count % 60 == 1 {
                                log::info!(
                                    "[logger] Frame #{}: {}x{}, {} bytes",
                                    frame_count,
                                    frame.width,
                                    frame.height,
                                    frame.data.len()
                                );
                            }
                        }
                        Err(_) => break,
                    }
                }
                recv(ctx.event_rx) -> msg => {
                    match msg {
                        Ok(event) => {
                            log::info!("[logger] Event: {:?}", event);
                        }
                        Err(_) => break,
                    }
                }
            }
        }
        log::info!("[logger] Shutting down after {} frames", frame_count);
    }

    fn stop(&self) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use shadowcast_core::Frame;
    use std::sync::Arc;
    use std::time::Instant;

    #[test]
    fn logger_plugin_receives_and_counts_frames() {
        let (frame_tx, frame_rx) = crossbeam_channel::bounded(4);
        let (_event_tx, event_rx) = crossbeam_channel::bounded(4);
        let (command_tx, _command_rx) = crossbeam_channel::bounded(4);

        let ctx = PluginContext {
            frame_rx,
            event_rx,
            command_tx,
            config: toml::Table::new(),
            stop_flag: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };

        let handle = std::thread::spawn(move || {
            let mut plugin = LoggerPlugin;
            plugin.run(ctx);
        });

        for _ in 0..3 {
            let frame = Arc::new(Frame {
                width: 320,
                height: 240,
                data: Arc::new(vec![0u8; 320 * 240 * 3]),
                timestamp: Instant::now(),
            });
            frame_tx.send(frame).unwrap();
        }

        drop(frame_tx);
        handle.join().unwrap();
    }
}
