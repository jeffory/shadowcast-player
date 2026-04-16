use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

use crossbeam_channel::Sender;
use shadowcast_core::{AppCommand, AppEvent, Frame, Plugin, PluginContext};

struct PluginHandle {
    name: String,
    thread: Option<JoinHandle<()>>,
    stop_flag: Arc<AtomicBool>,
}

pub struct PluginHost {
    handles: Vec<PluginHandle>,
    frame_txs: Vec<Sender<Arc<Frame>>>,
    event_txs: Vec<Sender<AppEvent>>,
    command_rx: crossbeam_channel::Receiver<AppCommand>,
    command_tx: Sender<AppCommand>,
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
        let (event_tx, event_rx) = crossbeam_channel::bounded(64);
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
        });
    }

    pub fn distribute_frame(&self, frame: Arc<Frame>) {
        for tx in &self.frame_txs {
            let _ = tx.try_send(Arc::clone(&frame));
        }
    }

    pub fn distribute_event(&self, event: AppEvent) {
        for tx in &self.event_txs {
            let _ = tx.try_send(event.clone());
        }
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
            data: vec![0u8; 320 * 240 * 3],
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
}
