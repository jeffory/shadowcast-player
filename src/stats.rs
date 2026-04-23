use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Shared frame-pipeline counters. Updated from the capture thread and the
/// render thread; read once per second by the main thread to produce a
/// snapshot for logging and on-screen display.
#[derive(Debug, Default)]
pub struct FrameStats {
    /// Frames delivered by the capture backend.
    pub captured: AtomicU64,
    /// Frames dropped by the capture backend because the render thread
    /// couldn't keep up (`try_send` returned `Err` on a full channel).
    pub dropped_at_capture: AtomicU64,
    /// Frames that reached the GPU and were presented.
    pub rendered: AtomicU64,
    /// Times the render thread found the frame channel empty — i.e. we were
    /// waiting on the capture side.
    pub recv_stalled: AtomicU64,
    /// Peak duration of the `RedrawRequested` handler in microseconds, since
    /// the last snapshot. `StatsTicker` swaps this to 0 each tick.
    pub peak_frame_us: AtomicU64,
}

impl FrameStats {
    pub fn inc_captured(&self) {
        self.captured.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_dropped_at_capture(&self) {
        self.dropped_at_capture.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_rendered(&self) {
        self.rendered.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_recv_stalled(&self) {
        self.recv_stalled.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_frame_us(&self, us: u64) {
        self.peak_frame_us.fetch_max(us, Ordering::Relaxed);
    }
}

/// One second's worth of deltas derived from `FrameStats`.
#[derive(Debug, Clone, Copy, Default)]
pub struct StatsSnapshot {
    pub captured_per_sec: u64,
    pub dropped_per_sec: u64,
    pub rendered_per_sec: u64,
    pub recv_stalled_per_sec: u64,
    pub peak_frame_us: u64,
}

/// Tracks previous counter values and returns per-second deltas when a full
/// tick has elapsed.
pub struct StatsTicker {
    interval: Duration,
    last_tick: Instant,
    last_captured: u64,
    last_dropped: u64,
    last_rendered: u64,
    last_recv_stalled: u64,
}

impl StatsTicker {
    pub fn new() -> Self {
        Self {
            interval: Duration::from_secs(1),
            last_tick: Instant::now(),
            last_captured: 0,
            last_dropped: 0,
            last_rendered: 0,
            last_recv_stalled: 0,
        }
    }

    /// Returns a snapshot of the last full interval, or `None` if the interval
    /// hasn't elapsed yet.
    pub fn tick(&mut self, stats: &FrameStats) -> Option<StatsSnapshot> {
        if self.last_tick.elapsed() < self.interval {
            return None;
        }

        let captured = stats.captured.load(Ordering::Relaxed);
        let dropped = stats.dropped_at_capture.load(Ordering::Relaxed);
        let rendered = stats.rendered.load(Ordering::Relaxed);
        let recv_stalled = stats.recv_stalled.load(Ordering::Relaxed);
        let peak_frame_us = stats.peak_frame_us.swap(0, Ordering::Relaxed);

        let snap = StatsSnapshot {
            captured_per_sec: captured.saturating_sub(self.last_captured),
            dropped_per_sec: dropped.saturating_sub(self.last_dropped),
            rendered_per_sec: rendered.saturating_sub(self.last_rendered),
            recv_stalled_per_sec: recv_stalled.saturating_sub(self.last_recv_stalled),
            peak_frame_us,
        };

        self.last_captured = captured;
        self.last_dropped = dropped;
        self.last_rendered = rendered;
        self.last_recv_stalled = recv_stalled;
        self.last_tick = Instant::now();

        Some(snap)
    }
}

impl Default for StatsTicker {
    fn default() -> Self {
        Self::new()
    }
}

impl StatsSnapshot {
    /// Formatted single-line summary for logging.
    pub fn summary(&self) -> String {
        format!(
            "captured={} rendered={} dropped_at_capture={} recv_stalled={} peak_frame={:.2}ms",
            self.captured_per_sec,
            self.rendered_per_sec,
            self.dropped_per_sec,
            self.recv_stalled_per_sec,
            self.peak_frame_us as f64 / 1000.0
        )
    }
}
