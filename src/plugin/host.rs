use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

use crossbeam_channel::Sender;
use shadowcast_core::{AppCommand, AppEvent, Frame, Plugin, PluginContext};

/// Size of each plugin's event channel. Sized to absorb keystroke bursts
/// (e.g. a held key auto-repeating at ~30 Hz while the plugin is busy) without
/// dropping events. 256 ≈ 8 seconds at 30 Hz before back-pressure.
const EVENT_CHANNEL_SIZE: usize = 256;

struct PluginHandle {
    name: String,
    thread: Option<JoinHandle<()>>,
    stop_flag: Arc<AtomicBool>,
    /// Cumulative count of events dropped because this plugin's channel was
    /// full. The host loop logs the running total at debug verbosity.
    dropped_events: Arc<AtomicU64>,
}

pub struct PluginHost {
    handles: Vec<PluginHandle>,
    frame_txs: Vec<Sender<Arc<Frame>>>,
    event_txs: Vec<Sender<AppEvent>>,
    command_rx: crossbeam_channel::Receiver<AppCommand>,
    command_tx: Sender<AppCommand>,
}

impl Default for PluginHost {
    fn default() -> Self {
        Self::new()
    }
}

impl PluginHost {
    pub fn new() -> Self {
        let (command_tx, command_rx) = crossbeam_channel::unbounded();
        Self {
            handles: Vec::new(),
            frame_txs: Vec::new(),
            event_txs: Vec::new(),
            command_rx,
            command_tx,
        }
    }

    pub fn register(&mut self, mut plugin: impl Plugin, config: toml::Table) {
        let (frame_tx, frame_rx) = crossbeam_channel::bounded(4);
        let (event_tx, event_rx) = crossbeam_channel::bounded(EVENT_CHANNEL_SIZE);
        let command_tx = self.command_tx.clone();

        let stop_flag = Arc::new(AtomicBool::new(false));
        let plugin_stop_flag = Arc::clone(&stop_flag);

        let ctx = PluginContext {
            frame_rx,
            event_rx,
            command_tx,
            config,
            stop_flag: Arc::clone(&stop_flag),
        };

        let name = plugin.name().to_string();
        let thread_name = format!("plugin-{}", name);

        let handle = std::thread::Builder::new()
            .name(thread_name)
            .spawn(move || {
                log::info!("Plugin '{}' started", plugin.name());
                plugin.run(ctx);
                log::info!("Plugin '{}' stopped", plugin.name());
            })
            .unwrap_or_else(|e| panic!("Failed to spawn plugin thread: {}", e));

        self.frame_txs.push(frame_tx);
        self.event_txs.push(event_tx);
        self.handles.push(PluginHandle {
            name,
            thread: Some(handle),
            stop_flag: plugin_stop_flag,
            dropped_events: Arc::new(AtomicU64::new(0)),
        });
    }

    pub fn distribute_frame(&self, frame: Arc<Frame>) {
        for tx in &self.frame_txs {
            let _ = tx.try_send(Arc::clone(&frame));
        }
    }

    pub fn distribute_event(&self, event: AppEvent) {
        for (tx, handle) in self.event_txs.iter().zip(self.handles.iter()) {
            if tx.try_send(event.clone()).is_err() {
                // Channel full — drop the newest event. We chose newest-drop
                // over oldest-drop because draining the channel from the
                // producer side would require an additional sync point on the
                // hot path; the queue is bursty so newest-drop keeps the
                // older context (e.g. the press) and only loses the tail.
                let prev = handle.dropped_events.fetch_add(1, Ordering::Relaxed);
                if prev == 0 || prev.is_power_of_two() {
                    log::debug!(
                        "plugin '{}' event channel full, dropped {} event(s) so far",
                        handle.name,
                        prev + 1
                    );
                }
            }
        }
    }

    /// Total events dropped for `plugin_name` due to a full channel. Returns
    /// `None` if no such plugin is registered. Primarily intended for tests
    /// and `RUST_LOG=debug` introspection.
    pub fn dropped_events(&self, plugin_name: &str) -> Option<u64> {
        self.handles
            .iter()
            .find(|h| h.name == plugin_name)
            .map(|h| h.dropped_events.load(Ordering::Relaxed))
    }

    pub fn poll_commands(&self) -> Vec<AppCommand> {
        let mut commands = Vec::new();
        while let Ok(cmd) = self.command_rx.try_recv() {
            commands.push(cmd);
        }
        commands
    }

    pub fn shutdown(&mut self) {
        for handle in &self.handles {
            handle.stop_flag.store(true, Ordering::SeqCst);
        }
        // Drop senders to unblock any plugins waiting on recv()
        self.frame_txs.clear();
        self.event_txs.clear();

        for handle in &mut self.handles {
            if let Some(thread) = handle.thread.take() {
                log::info!("Waiting for plugin '{}' to stop...", handle.name);
                if thread.join().is_err() {
                    log::error!("Plugin '{}' thread panicked", handle.name);
                }
            }
        }
    }
}

impl Drop for PluginHost {
    fn drop(&mut self) {
        if !self.handles.is_empty() {
            self.shutdown();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    struct CountingPlugin {
        frames_received: Arc<AtomicUsize>,
        events_received: Arc<AtomicUsize>,
        stop: Arc<AtomicBool>,
    }

    impl CountingPlugin {
        fn new() -> (Self, Arc<AtomicUsize>, Arc<AtomicUsize>) {
            let frames = Arc::new(AtomicUsize::new(0));
            let events = Arc::new(AtomicUsize::new(0));
            let stop = Arc::new(AtomicBool::new(false));
            (
                Self {
                    frames_received: Arc::clone(&frames),
                    events_received: Arc::clone(&events),
                    stop: Arc::clone(&stop),
                },
                frames,
                events,
            )
        }
    }

    impl Plugin for CountingPlugin {
        fn name(&self) -> &str {
            "counting"
        }

        fn run(&mut self, ctx: PluginContext) {
            loop {
                crossbeam_channel::select! {
                    recv(ctx.frame_rx) -> msg => {
                        match msg {
                            Ok(_) => { self.frames_received.fetch_add(1, Ordering::SeqCst); }
                            Err(_) => break,
                        }
                    }
                    recv(ctx.event_rx) -> msg => {
                        match msg {
                            Ok(_) => { self.events_received.fetch_add(1, Ordering::SeqCst); }
                            Err(_) => break,
                        }
                    }
                }
            }
        }

        fn stop(&self) {
            self.stop.store(true, Ordering::SeqCst);
        }
    }

    struct CommandPlugin {
        stop: Arc<AtomicBool>,
    }

    impl CommandPlugin {
        fn new() -> Self {
            Self {
                stop: Arc::new(AtomicBool::new(false)),
            }
        }
    }

    impl Plugin for CommandPlugin {
        fn name(&self) -> &str {
            "commander"
        }

        fn run(&mut self, ctx: PluginContext) {
            ctx.command_tx.send(AppCommand::TakeScreenshot).unwrap();
            while ctx.frame_rx.recv().is_ok() {}
        }

        fn stop(&self) {
            self.stop.store(true, Ordering::SeqCst);
        }
    }

    fn test_frame() -> Arc<Frame> {
        Arc::new(Frame {
            width: 320,
            height: 240,
            data: Arc::new(vec![0u8; 320 * 240 * 3]),
            timestamp: Instant::now(),
        })
    }

    #[test]
    fn host_new_has_no_plugins() {
        let host = PluginHost::new();
        assert!(host.handles.is_empty());
        assert!(host.poll_commands().is_empty());
    }

    #[test]
    fn plugin_receives_frames() {
        let (plugin, frames, _events) = CountingPlugin::new();
        let mut host = PluginHost::new();
        host.register(plugin, toml::Table::new());

        for _ in 0..3 {
            host.distribute_frame(test_frame());
        }

        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(frames.load(Ordering::SeqCst), 3);

        host.shutdown();
    }

    #[test]
    fn plugin_receives_events() {
        let (plugin, _frames, events) = CountingPlugin::new();
        let mut host = PluginHost::new();
        host.register(plugin, toml::Table::new());

        host.distribute_event(AppEvent::DeviceConnected {
            name: "test".into(),
        });
        host.distribute_event(AppEvent::DeviceDisconnected);

        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(events.load(Ordering::SeqCst), 2);

        host.shutdown();
    }

    #[test]
    fn plugin_can_send_commands() {
        let plugin = CommandPlugin::new();
        let mut host = PluginHost::new();
        host.register(plugin, toml::Table::new());

        std::thread::sleep(Duration::from_millis(50));
        let commands = host.poll_commands();
        assert_eq!(commands.len(), 1);
        assert!(matches!(commands[0], AppCommand::TakeScreenshot));

        host.shutdown();
    }

    #[test]
    fn slow_consumer_does_not_block_distribute() {
        struct SlowPlugin;
        impl Plugin for SlowPlugin {
            fn name(&self) -> &str {
                "slow"
            }
            fn run(&mut self, ctx: PluginContext) {
                while ctx.event_rx.recv().is_ok() {}
            }
            fn stop(&self) {}
        }

        let mut host = PluginHost::new();
        host.register(SlowPlugin, toml::Table::new());

        let start = Instant::now();
        for _ in 0..100 {
            host.distribute_frame(test_frame());
        }
        let elapsed = start.elapsed();

        assert!(elapsed < Duration::from_millis(100));

        host.shutdown();
    }

    #[test]
    fn multiple_plugins_receive_independently() {
        let (plugin_a, frames_a, _) = CountingPlugin::new();
        let (plugin_b, frames_b, _) = CountingPlugin::new();

        let mut host = PluginHost::new();
        host.register(plugin_a, toml::Table::new());
        host.register(plugin_b, toml::Table::new());

        host.distribute_frame(test_frame());

        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(frames_a.load(Ordering::SeqCst), 1);
        assert_eq!(frames_b.load(Ordering::SeqCst), 1);

        host.shutdown();
    }

    #[test]
    fn shutdown_joins_all_threads() {
        let (plugin, _, _) = CountingPlugin::new();
        let mut host = PluginHost::new();
        host.register(plugin, toml::Table::new());

        host.shutdown();
        for h in &host.handles {
            assert!(h.thread.is_none());
        }
    }

    #[test]
    fn capture_off_suppresses_keyboard_forwarding() {
        use crate::input::keyboard_event_for_plugins;
        use winit::keyboard::{KeyCode, ModifiersState, PhysicalKey};

        let (plugin, _frames, events) = CountingPlugin::new();
        let mut host = PluginHost::new();
        host.register(plugin, toml::Table::new());

        let toggle = PhysicalKey::Code(KeyCode::ControlRight);
        let key_a = PhysicalKey::Code(KeyCode::KeyA);

        // Capture OFF: the gating function returns None, so nothing is
        // distributed. We assert that explicitly.
        let suppressed =
            keyboard_event_for_plugins(false, key_a, toggle, ModifiersState::empty(), true, false);
        assert!(suppressed.is_none());

        // Capture ON: we get an AppEvent and forward it. The CountingPlugin
        // sees exactly one event.
        let forwarded =
            keyboard_event_for_plugins(true, key_a, toggle, ModifiersState::empty(), true, false)
                .expect("capture-on should produce an event");
        assert!(matches!(forwarded, AppEvent::KeyboardInput { .. }));
        host.distribute_event(forwarded);

        // The toggle key itself never forwards, even with capture on.
        let toggle_event =
            keyboard_event_for_plugins(true, toggle, toggle, ModifiersState::empty(), true, false);
        assert!(toggle_event.is_none());

        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(events.load(Ordering::SeqCst), 1);

        host.shutdown();
    }

    #[test]
    fn dropped_events_increment_when_channel_full() {
        // Slow plugin that never drains its event channel until shutdown,
        // so a small burst saturates the bounded queue and the host bumps
        // the drop counter for everything past EVENT_CHANNEL_SIZE.
        struct NeverDrainPlugin {
            stop: Arc<AtomicBool>,
        }
        impl Plugin for NeverDrainPlugin {
            fn name(&self) -> &str {
                "never-drain"
            }
            fn run(&mut self, _ctx: PluginContext) {
                while !self.stop.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
            fn stop(&self) {
                self.stop.store(true, Ordering::SeqCst);
            }
        }
        let stop = Arc::new(AtomicBool::new(false));
        let plugin = NeverDrainPlugin {
            stop: Arc::clone(&stop),
        };
        let mut host = PluginHost::new();
        host.register(plugin, toml::Table::new());

        // Push well over EVENT_CHANNEL_SIZE.
        let burst = EVENT_CHANNEL_SIZE + 20;
        for _ in 0..burst {
            host.distribute_event(AppEvent::DeviceDisconnected);
        }

        let dropped = host
            .dropped_events("never-drain")
            .expect("plugin should be registered");
        assert!(dropped >= 20, "expected >=20 drops, got {}", dropped);

        stop.store(true, Ordering::SeqCst);
        host.shutdown();
    }
}
