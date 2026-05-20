//! `shadowcast-plugin-pico-keeb` — forward captured keyboard / mouse input
//! to a [pico-keeb](https://github.com/jeffory/pico-keeb) RP2040 board over
//! a serial port, where it is replayed as a USB HID device to a downstream
//! target PC.
//!
//! Hot-path goals (per HOM-111):
//! - Keystroke → USB HID report on the target PC under ~5 ms on a quiet system.
//! - The plugin thread *never* blocks on serial I/O; a dedicated writer
//!   thread drains a small bounded queue. See [`link`].
//! - The plugin only forwards while the host is in capture mode. On
//!   `CaptureModeChanged { active: false }` we send `Frame::Reset` and
//!   clear local held state so a key that was down when capture ended
//!   doesn't get stuck on the target.

pub mod link;
pub mod mapping;

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use crossbeam_channel::select;
use pico_keeb_protocol::binary::Frame;
use shadowcast_core::{AppEvent, Plugin, PluginContext};

use crate::link::{FrameSink, SerialFrameSink};
use crate::mapping::{hid_usage_to_key_byte, mouse_button_bit, split_i8};

const DEFAULT_BAUD: u32 = 921_600;

/// Wall clock used by the run loop. Lifted into a constant so the periodic
/// stop-flag check is easy to reason about and easy to bump if needed.
const STOP_POLL_INTERVAL: Duration = Duration::from_millis(50);

#[derive(Debug, Clone)]
struct PicoKeebConfig {
    port: String,
    baud: u32,
    forward_mouse: bool,
    key_hold_ms: u64,
}

impl PicoKeebConfig {
    fn from_table(table: &toml::Table) -> Result<Self> {
        let port = table
            .get("port")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("missing required 'port' string"))?
            .to_string();
        let baud = table
            .get("baud")
            .and_then(|v| v.as_integer())
            .map(|i| i.max(0) as u32)
            .unwrap_or(DEFAULT_BAUD);
        let forward_mouse = table
            .get("forward_mouse")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let key_hold_ms = table
            .get("key_hold_ms")
            .and_then(|v| v.as_integer())
            .map(|i| i.max(0) as u64)
            .unwrap_or(0);
        Ok(Self {
            port,
            baud,
            forward_mouse,
            key_hold_ms,
        })
    }
}

/// 6KRO USB HID keyboard report state, plus mouse-button + sub-pixel motion
/// accumulators. Lives in its own struct so the state machine is testable
/// against an in-memory [`FrameSink`].
#[derive(Debug, Default, Clone)]
pub(crate) struct HeldState {
    modifiers: u8,
    keys: [u8; 6],
    buttons: u8,
    mouse_acc_x: f32,
    mouse_acc_y: f32,
    /// Pending auto-releases. Only used when `key_hold_ms > 0` (MiSTer-
    /// style targets that need chatter-filter taps). Capped by 6KRO at
    /// 6 entries, so a linear scan is fine.
    pending_releases: Vec<(Instant, u8)>,
}

impl HeldState {
    fn new() -> Self {
        Self::default()
    }

    fn press_key(&mut self, code: u8) -> bool {
        if self.keys.contains(&code) {
            return true;
        }
        for slot in self.keys.iter_mut() {
            if *slot == 0 {
                *slot = code;
                return true;
            }
        }
        false
    }

    fn release_key(&mut self, code: u8) {
        for slot in self.keys.iter_mut() {
            if *slot == code {
                *slot = 0;
            }
        }
    }

    fn reset(&mut self) {
        self.modifiers = 0;
        self.keys = [0; 6];
        self.buttons = 0;
        self.mouse_acc_x = 0.0;
        self.mouse_acc_y = 0.0;
        self.pending_releases.clear();
    }

    fn kbd_frame(&self) -> Frame {
        Frame::Kbd {
            modifiers: self.modifiers,
            keys: self.keys,
        }
    }

    /// Refresh (or insert) the auto-release deadline for `code`. A re-press
    /// of an already-held key pushes the deadline out, matching the
    /// firmware's "the user is still holding it" intent.
    fn schedule_release(&mut self, code: u8, deadline: Instant) {
        for entry in self.pending_releases.iter_mut() {
            if entry.1 == code {
                entry.0 = deadline;
                return;
            }
        }
        self.pending_releases.push((deadline, code));
    }

    /// Drain any auto-releases whose deadline has expired and emit a single
    /// `Frame::Kbd` reflecting the new held set. Returns the next pending
    /// deadline, if any, so the run loop knows when to wake.
    fn process_pending_releases<S: FrameSink + ?Sized>(
        &mut self,
        now: Instant,
        sink: &S,
    ) -> Option<Instant> {
        let mut emitted = false;
        let mut i = 0;
        while i < self.pending_releases.len() {
            if self.pending_releases[i].0 <= now {
                let (_, code) = self.pending_releases.swap_remove(i);
                self.release_key(code);
                emitted = true;
            } else {
                i += 1;
            }
        }
        if emitted {
            sink.send(self.kbd_frame());
        }
        self.pending_releases.iter().map(|(d, _)| *d).min()
    }
}

/// Apply one `AppEvent` to `state` and emit any resulting HID frames via
/// `sink`. Pure-ish: the only side-effects are `sink.send(...)` calls and
/// reading `Instant::now()` (when `key_hold` is set). Made `pub(crate)` so
/// the unit tests can drive it with a `RecordingSink`.
pub(crate) fn handle_event<S: FrameSink + ?Sized>(
    state: &mut HeldState,
    event: &AppEvent,
    sink: &S,
    key_hold: Option<Duration>,
    forward_mouse: bool,
) {
    match *event {
        AppEvent::CaptureModeChanged { active: _ } => {
            // Always Reset + clear local state, both on activate and on
            // deactivate. On activate we send Reset so any stale state on
            // the firmware side from a previous session is cleared; on
            // deactivate we send Reset so a key that was down when the
            // user toggled out of capture doesn't stay stuck on the target.
            state.reset();
            sink.send(Frame::Reset);
        }
        AppEvent::KeyboardInput {
            key_code,
            modifiers,
            pressed,
            repeat,
            ..
        } => {
            // The host already merged the modifier bitmask for us — adopt
            // it verbatim on every keyboard event.
            state.modifiers = modifiers;

            let Some(code) = hid_usage_to_key_byte(key_code) else {
                // Modifier-only event (or unknown / unmapped key). For
                // modifier presses + releases, emit a fresh Kbd so the
                // updated mask reaches the firmware. Skip repeats — those
                // carry no new information.
                if !repeat {
                    sink.send(state.kbd_frame());
                }
                return;
            };

            if pressed {
                if repeat {
                    // Default: firmware handles auto-repeat, so we drop
                    // OS-side repeat events. In key_hold mode the scheduled
                    // auto-release also makes repeats redundant — same
                    // behaviour.
                    return;
                }
                if !state.press_key(code) {
                    log::trace!(
                        "[pico-keeb] 6KRO full, dropping press of HID 0x{:02X}",
                        code
                    );
                    return;
                }
                sink.send(state.kbd_frame());
                if let Some(hold) = key_hold {
                    state.schedule_release(code, Instant::now() + hold);
                }
            } else {
                // Release. In key_hold mode the scheduled deadline owns
                // the release — ignore the OS-side release entirely so a
                // brief tap still holds for the configured duration.
                if key_hold.is_some() {
                    return;
                }
                state.release_key(code);
                sink.send(state.kbd_frame());
            }
        }
        AppEvent::MouseMotion { dx, dy } => {
            if !forward_mouse {
                return;
            }
            // Accumulate sub-pixel fractions across calls. If |total| > 127
            // on either axis we emit multiple frames in a row so the motion
            // isn't truncated to a single saturating step.
            let mut x = state.mouse_acc_x + dx;
            let mut y = state.mouse_acc_y + dy;
            loop {
                let (sx, rx) = split_i8(x);
                let (sy, ry) = split_i8(y);
                if sx == 0 && sy == 0 {
                    state.mouse_acc_x = rx;
                    state.mouse_acc_y = ry;
                    break;
                }
                sink.send(Frame::Mouse {
                    buttons: state.buttons,
                    dx: sx,
                    dy: sy,
                    wheel: 0,
                });
                x = rx;
                y = ry;
            }
        }
        AppEvent::MouseButton { button, pressed } => {
            if !forward_mouse {
                return;
            }
            let bit = mouse_button_bit(button);
            if pressed {
                state.buttons |= bit;
            } else {
                state.buttons &= !bit;
            }
            sink.send(Frame::Mouse {
                buttons: state.buttons,
                dx: 0,
                dy: 0,
                wheel: 0,
            });
        }
        AppEvent::MouseScroll { dx: _, dy } => {
            if !forward_mouse {
                return;
            }
            let (wheel, _residual) = split_i8(dy);
            if wheel == 0 {
                return;
            }
            sink.send(Frame::Mouse {
                buttons: state.buttons,
                dx: 0,
                dy: 0,
                wheel,
            });
        }
        // All other AppEvent variants are deliberately ignored: device /
        // recording / format / resize / audio events have nothing to say
        // to the HID forwarder.
        _ => {}
    }
}

pub struct PicoKeebPlugin;

impl Plugin for PicoKeebPlugin {
    fn name(&self) -> &str {
        "pico-keeb"
    }

    fn run(&mut self, ctx: PluginContext) {
        let cfg = match PicoKeebConfig::from_table(&ctx.config) {
            Ok(c) => c,
            Err(e) => {
                log::error!("[pico-keeb] invalid config: {:#}. Plugin not starting.", e);
                return;
            }
        };

        let sink = match SerialFrameSink::open(&cfg.port, cfg.baud) {
            Ok(s) => s,
            Err(e) => {
                // Per host expectation, exit cleanly on open failure —
                // never panic, other plugins are still running.
                log::error!("[pico-keeb] {:#}. Plugin not starting.", e);
                return;
            }
        };

        log::info!(
            "[pico-keeb] forwarding HID frames to {} @ {} baud (mouse={}, key_hold={}ms)",
            cfg.port,
            cfg.baud,
            cfg.forward_mouse,
            cfg.key_hold_ms
        );

        let key_hold = if cfg.key_hold_ms > 0 {
            Some(Duration::from_millis(cfg.key_hold_ms))
        } else {
            None
        };

        let mut state = HeldState::new();

        loop {
            if ctx.stop_flag.load(Ordering::Relaxed) {
                break;
            }

            // Service auto-releases at the top of each iteration so the
            // timeout we feed to `select!` can be tightened to the next
            // pending deadline.
            let next_release = state.process_pending_releases(Instant::now(), &sink);
            let timeout = match next_release {
                Some(deadline) => deadline
                    .saturating_duration_since(Instant::now())
                    .min(STOP_POLL_INTERVAL),
                None => STOP_POLL_INTERVAL,
            };

            select! {
                recv(ctx.frame_rx) -> msg => {
                    // Frames are dropped here so we don't pin Arc<Vec<u8>>
                    // values forever on a host with no other consumer of
                    // this plugin's frame channel. A closed channel means
                    // the host is shutting us down.
                    if msg.is_err() {
                        break;
                    }
                }
                recv(ctx.event_rx) -> msg => match msg {
                    Ok(event) => handle_event(&mut state, &event, &sink, key_hold, cfg.forward_mouse),
                    Err(_) => break,
                },
                default(timeout) => {}
            }
        }

        // Drain any straggler auto-releases the host shutdown raced past,
        // then emit one Reset so we don't leave the target with stuck keys.
        state.reset();
        sink.send(Frame::Reset);
        sink.shutdown();
        log::info!("[pico-keeb] stopped");
    }

    fn stop(&self) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::link::test_support::RecordingSink;
    use shadowcast_core::modifier::{MOD_LCTRL, MOD_LSHIFT};
    use shadowcast_core::MouseButton;

    fn key_press(usage: u32, modifiers: u8) -> AppEvent {
        AppEvent::KeyboardInput {
            key_code: usage,
            scan_code: 0,
            modifiers,
            pressed: true,
            repeat: false,
        }
    }

    fn key_release(usage: u32, modifiers: u8) -> AppEvent {
        AppEvent::KeyboardInput {
            key_code: usage,
            scan_code: 0,
            modifiers,
            pressed: false,
            repeat: false,
        }
    }

    fn key_repeat(usage: u32, modifiers: u8) -> AppEvent {
        AppEvent::KeyboardInput {
            key_code: usage,
            scan_code: 0,
            modifiers,
            pressed: true,
            repeat: true,
        }
    }

    #[test]
    fn config_parses_required_and_defaults() {
        let table: toml::Table = r#"
            port = "/dev/ttyUSB0"
        "#
        .parse()
        .unwrap();
        let cfg = PicoKeebConfig::from_table(&table).unwrap();
        assert_eq!(cfg.port, "/dev/ttyUSB0");
        assert_eq!(cfg.baud, DEFAULT_BAUD);
        assert!(cfg.forward_mouse);
        assert_eq!(cfg.key_hold_ms, 0);
    }

    #[test]
    fn config_parses_overrides() {
        let table: toml::Table = r#"
            port = "/dev/ttyACM0"
            baud = 115200
            forward_mouse = false
            key_hold_ms = 30
        "#
        .parse()
        .unwrap();
        let cfg = PicoKeebConfig::from_table(&table).unwrap();
        assert_eq!(cfg.port, "/dev/ttyACM0");
        assert_eq!(cfg.baud, 115_200);
        assert!(!cfg.forward_mouse);
        assert_eq!(cfg.key_hold_ms, 30);
    }

    #[test]
    fn config_missing_port_errors() {
        let table = toml::Table::new();
        assert!(PicoKeebConfig::from_table(&table).is_err());
    }

    #[test]
    fn press_then_release_clears_held_keys() {
        let sink = RecordingSink::new();
        let mut state = HeldState::new();

        // A = 0x04
        handle_event(&mut state, &key_press(0x04, 0), &sink, None, true);
        assert_eq!(state.keys[0], 0x04);
        assert!(matches!(sink.frames().last(), Some(Frame::Kbd { keys, .. }) if keys[0] == 0x04));

        handle_event(&mut state, &key_release(0x04, 0), &sink, None, true);
        assert_eq!(state.keys, [0u8; 6]);
        assert!(matches!(sink.frames().last(), Some(Frame::Kbd { keys, .. }) if *keys == [0u8; 6]));
    }

    #[test]
    fn seventh_key_is_dropped_6kro() {
        let sink = RecordingSink::new();
        let mut state = HeldState::new();
        // Press 6 distinct keys (HID usages 0x04..=0x09 = A..F).
        for code in 0x04u32..0x0A {
            handle_event(&mut state, &key_press(code, 0), &sink, None, true);
        }
        assert_eq!(state.keys, [0x04, 0x05, 0x06, 0x07, 0x08, 0x09]);
        let frames_before = sink.frames().len();

        // 7th distinct key (G = 0x0A) — must be dropped, no new frame.
        handle_event(&mut state, &key_press(0x0A, 0), &sink, None, true);
        assert_eq!(state.keys, [0x04, 0x05, 0x06, 0x07, 0x08, 0x09]);
        assert_eq!(
            sink.frames().len(),
            frames_before,
            "7th key press must not emit a frame"
        );
    }

    #[test]
    fn capture_mode_off_emits_single_reset_and_clears_state() {
        let sink = RecordingSink::new();
        let mut state = HeldState::new();
        // Pretend we're holding Shift + A + the left mouse button.
        state.modifiers = MOD_LSHIFT;
        state.keys[0] = 0x04;
        state.buttons = 0x01;
        state.mouse_acc_x = 12.7;

        let before = sink.frames().len();
        handle_event(
            &mut state,
            &AppEvent::CaptureModeChanged { active: false },
            &sink,
            None,
            true,
        );

        assert_eq!(state.modifiers, 0);
        assert_eq!(state.keys, [0u8; 6]);
        assert_eq!(state.buttons, 0);
        assert_eq!(state.mouse_acc_x, 0.0);

        let after = sink.frames();
        assert_eq!(after.len(), before + 1);
        assert!(matches!(after.last(), Some(Frame::Reset)));
    }

    #[test]
    fn capture_mode_on_also_emits_reset() {
        let sink = RecordingSink::new();
        let mut state = HeldState::new();
        handle_event(
            &mut state,
            &AppEvent::CaptureModeChanged { active: true },
            &sink,
            None,
            true,
        );
        assert!(matches!(sink.frames().last(), Some(Frame::Reset)));
    }

    #[test]
    fn modifier_only_event_emits_kbd_with_new_mask() {
        let sink = RecordingSink::new();
        let mut state = HeldState::new();

        // LCtrl press (HID usage 0xE0, host already merged modifiers=MOD_LCTRL).
        handle_event(&mut state, &key_press(0xE0, MOD_LCTRL), &sink, None, true);

        assert_eq!(state.modifiers, MOD_LCTRL);
        assert_eq!(state.keys, [0u8; 6]);
        match sink.frames().last() {
            Some(Frame::Kbd { modifiers, keys }) => {
                assert_eq!(*modifiers, MOD_LCTRL);
                assert_eq!(*keys, [0u8; 6]);
            }
            other => panic!("expected Kbd frame, got {:?}", other),
        }
    }

    #[test]
    fn modifier_carries_through_subsequent_key() {
        let sink = RecordingSink::new();
        let mut state = HeldState::new();
        // User is holding Ctrl, then presses S (Ctrl+S).
        handle_event(&mut state, &key_press(0x16, MOD_LCTRL), &sink, None, true);
        match sink.frames().last() {
            Some(Frame::Kbd { modifiers, keys }) => {
                assert_eq!(*modifiers, MOD_LCTRL);
                assert_eq!(keys[0], 0x16);
            }
            other => panic!("expected Kbd, got {:?}", other),
        }
    }

    #[test]
    fn key_repeats_are_skipped_by_default() {
        let sink = RecordingSink::new();
        let mut state = HeldState::new();
        handle_event(&mut state, &key_press(0x04, 0), &sink, None, true);
        let after_press = sink.frames().len();

        // Three auto-repeats — none should produce frames.
        for _ in 0..3 {
            handle_event(&mut state, &key_repeat(0x04, 0), &sink, None, true);
        }
        assert_eq!(sink.frames().len(), after_press);
    }

    #[test]
    fn key_hold_mode_schedules_auto_release() {
        let sink = RecordingSink::new();
        let mut state = HeldState::new();
        let hold = Duration::from_millis(5);

        // Press triggers an immediate Kbd frame AND a scheduled release.
        handle_event(&mut state, &key_press(0x04, 0), &sink, Some(hold), true);
        assert_eq!(state.keys[0], 0x04);
        assert_eq!(state.pending_releases.len(), 1);

        // OS-side release is ignored in hold mode — held set unchanged.
        handle_event(&mut state, &key_release(0x04, 0), &sink, Some(hold), true);
        assert_eq!(state.keys[0], 0x04);
        assert_eq!(state.pending_releases.len(), 1);

        // Wait past the deadline and process pending: key is released.
        std::thread::sleep(Duration::from_millis(10));
        let next = state.process_pending_releases(Instant::now(), &sink);
        assert!(next.is_none());
        assert_eq!(state.keys, [0u8; 6]);
        assert!(matches!(sink.frames().last(), Some(Frame::Kbd { keys, .. }) if *keys == [0u8; 6]));
    }

    #[test]
    fn key_hold_repress_refreshes_deadline() {
        let sink = RecordingSink::new();
        let mut state = HeldState::new();
        let hold = Duration::from_millis(50);

        handle_event(&mut state, &key_press(0x04, 0), &sink, Some(hold), true);
        assert_eq!(state.pending_releases.len(), 1);
        let first_deadline = state.pending_releases[0].0;

        std::thread::sleep(Duration::from_millis(5));
        handle_event(&mut state, &key_press(0x04, 0), &sink, Some(hold), true);
        assert_eq!(state.pending_releases.len(), 1);
        assert!(
            state.pending_releases[0].0 > first_deadline,
            "re-press must refresh the deadline forward"
        );
    }

    #[test]
    fn mouse_motion_accumulates_subpixel_residual() {
        let sink = RecordingSink::new();
        let mut state = HeldState::new();

        // Three sub-pixel deltas of 0.4 add to 1.2 → one i8 step of 1 + 0.2 residual.
        for _ in 0..3 {
            handle_event(
                &mut state,
                &AppEvent::MouseMotion { dx: 0.4, dy: 0.0 },
                &sink,
                None,
                true,
            );
        }
        let mouse_frames: Vec<_> = sink
            .frames()
            .into_iter()
            .filter(|f| matches!(f, Frame::Mouse { .. }))
            .collect();
        // Expect exactly one Mouse frame with dx=1 (after the 3rd step rolls over).
        assert_eq!(mouse_frames.len(), 1);
        assert!(
            matches!(mouse_frames[0], Frame::Mouse { dx: 1, dy: 0, .. }),
            "got {:?}",
            mouse_frames[0]
        );
        assert!((state.mouse_acc_x - 0.2).abs() < 1e-4);
    }

    #[test]
    fn mouse_motion_chunks_oversized_delta() {
        let sink = RecordingSink::new();
        let mut state = HeldState::new();
        // 200 px on x should produce two frames: i8::MAX (127) + 73.
        handle_event(
            &mut state,
            &AppEvent::MouseMotion { dx: 200.0, dy: 0.0 },
            &sink,
            None,
            true,
        );
        let mouse_frames: Vec<_> = sink
            .frames()
            .into_iter()
            .filter(|f| matches!(f, Frame::Mouse { .. }))
            .collect();
        assert_eq!(mouse_frames.len(), 2);
        assert!(matches!(
            mouse_frames[0],
            Frame::Mouse { dx: 127, dy: 0, .. }
        ));
        assert!(matches!(
            mouse_frames[1],
            Frame::Mouse { dx: 73, dy: 0, .. }
        ));
    }

    #[test]
    fn mouse_motion_respects_forward_mouse_false() {
        let sink = RecordingSink::new();
        let mut state = HeldState::new();
        handle_event(
            &mut state,
            &AppEvent::MouseMotion {
                dx: 100.0,
                dy: 100.0,
            },
            &sink,
            None,
            false,
        );
        assert!(sink.frames().is_empty());
    }

    #[test]
    fn mouse_button_toggles_buttons_bitmask() {
        let sink = RecordingSink::new();
        let mut state = HeldState::new();
        handle_event(
            &mut state,
            &AppEvent::MouseButton {
                button: MouseButton::Right,
                pressed: true,
            },
            &sink,
            None,
            true,
        );
        assert_eq!(state.buttons, 0x02);
        handle_event(
            &mut state,
            &AppEvent::MouseButton {
                button: MouseButton::Right,
                pressed: false,
            },
            &sink,
            None,
            true,
        );
        assert_eq!(state.buttons, 0x00);
        let mouse_frames: Vec<_> = sink
            .frames()
            .into_iter()
            .filter(|f| matches!(f, Frame::Mouse { .. }))
            .collect();
        assert_eq!(mouse_frames.len(), 2);
    }

    #[test]
    fn mouse_scroll_emits_wheel_only_when_nonzero() {
        let sink = RecordingSink::new();
        let mut state = HeldState::new();
        // Sub-line scroll → no frame.
        handle_event(
            &mut state,
            &AppEvent::MouseScroll { dx: 0.0, dy: 0.3 },
            &sink,
            None,
            true,
        );
        assert!(sink.frames().is_empty());

        // 1.0 line scroll up → one frame with wheel=1.
        handle_event(
            &mut state,
            &AppEvent::MouseScroll { dx: 0.0, dy: 1.0 },
            &sink,
            None,
            true,
        );
        assert!(matches!(
            sink.frames().last(),
            Some(Frame::Mouse {
                dx: 0,
                dy: 0,
                wheel: 1,
                ..
            })
        ));
    }
}
