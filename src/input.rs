//! Translation between winit input types and the pico-keeb-protocol values
//! forwarded over [`AppEvent`]. Lives in its own module so the hot path can
//! be a single `match` per event and so the mapping is unit-testable without
//! driving a real winit event loop.
//!
//! All public functions here are `#[inline]`-friendly and allocation-free —
//! the keyboard hot path runs on the render thread, and the issue's latency
//! budget is sub-millisecond per event.

use shadowcast_core::modifier::*;
use shadowcast_core::{AppEvent, MouseButton as CoreMouseButton};
use winit::event::MouseButton as WinitMouseButton;
use winit::keyboard::{KeyCode, ModifiersState, PhysicalKey};

/// Map a winit [`ModifiersState`] snapshot to the pico-keeb-protocol modifier
/// bitmask. `ModifiersState` does not distinguish left and right modifier
/// keys, so this collapses every held modifier to its **left-side** bit
/// (e.g. `SHIFT` → `MOD_LSHIFT`). For modifier-key *press* events the host
/// also looks at the [`PhysicalKey`] directly via
/// [`physical_key_modifier_bit`] to recover the correct side bit.
pub fn modifiers_state_to_pico_keeb_mask(state: ModifiersState) -> u8 {
    let mut mask = 0u8;
    if state.control_key() {
        mask |= MOD_LCTRL;
    }
    if state.shift_key() {
        mask |= MOD_LSHIFT;
    }
    if state.alt_key() {
        mask |= MOD_LALT;
    }
    if state.super_key() {
        mask |= MOD_LGUI;
    }
    mask
}

/// If `key` is a modifier key, return its pico-keeb-protocol bit (with the
/// correct left/right side). For non-modifier keys, returns `None`.
pub fn physical_key_modifier_bit(key: PhysicalKey) -> Option<u8> {
    match key {
        PhysicalKey::Code(KeyCode::ControlLeft) => Some(MOD_LCTRL),
        PhysicalKey::Code(KeyCode::ShiftLeft) => Some(MOD_LSHIFT),
        PhysicalKey::Code(KeyCode::AltLeft) => Some(MOD_LALT),
        PhysicalKey::Code(KeyCode::SuperLeft) => Some(MOD_LGUI),
        PhysicalKey::Code(KeyCode::ControlRight) => Some(MOD_RCTRL),
        PhysicalKey::Code(KeyCode::ShiftRight) => Some(MOD_RSHIFT),
        PhysicalKey::Code(KeyCode::AltRight) => Some(MOD_RALT),
        PhysicalKey::Code(KeyCode::SuperRight) => Some(MOD_RGUI),
        _ => None,
    }
}

/// Translate a winit [`PhysicalKey`] to a USB HID Usage ID on the Keyboard /
/// Keypad page (`0x07`). Unrecognised keys (and `PhysicalKey::Unidentified`)
/// return `0`, which downstream plugins should treat as "unknown key —
/// consult `scan_code` instead". The table covers every key the pico-keeb
/// plugin is expected to forward; extending it is intentionally cheap (one
/// match arm per key, compiled to a jump table).
pub fn physical_key_to_hid_usage(key: PhysicalKey) -> u32 {
    let code = match key {
        PhysicalKey::Code(c) => c,
        PhysicalKey::Unidentified(_) => return 0,
    };
    match code {
        // Letters
        KeyCode::KeyA => 0x04,
        KeyCode::KeyB => 0x05,
        KeyCode::KeyC => 0x06,
        KeyCode::KeyD => 0x07,
        KeyCode::KeyE => 0x08,
        KeyCode::KeyF => 0x09,
        KeyCode::KeyG => 0x0A,
        KeyCode::KeyH => 0x0B,
        KeyCode::KeyI => 0x0C,
        KeyCode::KeyJ => 0x0D,
        KeyCode::KeyK => 0x0E,
        KeyCode::KeyL => 0x0F,
        KeyCode::KeyM => 0x10,
        KeyCode::KeyN => 0x11,
        KeyCode::KeyO => 0x12,
        KeyCode::KeyP => 0x13,
        KeyCode::KeyQ => 0x14,
        KeyCode::KeyR => 0x15,
        KeyCode::KeyS => 0x16,
        KeyCode::KeyT => 0x17,
        KeyCode::KeyU => 0x18,
        KeyCode::KeyV => 0x19,
        KeyCode::KeyW => 0x1A,
        KeyCode::KeyX => 0x1B,
        KeyCode::KeyY => 0x1C,
        KeyCode::KeyZ => 0x1D,
        // Digits (top row)
        KeyCode::Digit1 => 0x1E,
        KeyCode::Digit2 => 0x1F,
        KeyCode::Digit3 => 0x20,
        KeyCode::Digit4 => 0x21,
        KeyCode::Digit5 => 0x22,
        KeyCode::Digit6 => 0x23,
        KeyCode::Digit7 => 0x24,
        KeyCode::Digit8 => 0x25,
        KeyCode::Digit9 => 0x26,
        KeyCode::Digit0 => 0x27,
        // Editing
        KeyCode::Enter => 0x28,
        KeyCode::Escape => 0x29,
        KeyCode::Backspace => 0x2A,
        KeyCode::Tab => 0x2B,
        KeyCode::Space => 0x2C,
        KeyCode::Minus => 0x2D,
        KeyCode::Equal => 0x2E,
        KeyCode::BracketLeft => 0x2F,
        KeyCode::BracketRight => 0x30,
        KeyCode::Backslash => 0x31,
        KeyCode::Semicolon => 0x33,
        KeyCode::Quote => 0x34,
        KeyCode::Backquote => 0x35,
        KeyCode::Comma => 0x36,
        KeyCode::Period => 0x37,
        KeyCode::Slash => 0x38,
        KeyCode::CapsLock => 0x39,
        // F-row
        KeyCode::F1 => 0x3A,
        KeyCode::F2 => 0x3B,
        KeyCode::F3 => 0x3C,
        KeyCode::F4 => 0x3D,
        KeyCode::F5 => 0x3E,
        KeyCode::F6 => 0x3F,
        KeyCode::F7 => 0x40,
        KeyCode::F8 => 0x41,
        KeyCode::F9 => 0x42,
        KeyCode::F10 => 0x43,
        KeyCode::F11 => 0x44,
        KeyCode::F12 => 0x45,
        KeyCode::PrintScreen => 0x46,
        KeyCode::ScrollLock => 0x47,
        KeyCode::Pause => 0x48,
        // Navigation
        KeyCode::Insert => 0x49,
        KeyCode::Home => 0x4A,
        KeyCode::PageUp => 0x4B,
        KeyCode::Delete => 0x4C,
        KeyCode::End => 0x4D,
        KeyCode::PageDown => 0x4E,
        KeyCode::ArrowRight => 0x4F,
        KeyCode::ArrowLeft => 0x50,
        KeyCode::ArrowDown => 0x51,
        KeyCode::ArrowUp => 0x52,
        // Keypad
        KeyCode::NumLock => 0x53,
        KeyCode::NumpadDivide => 0x54,
        KeyCode::NumpadMultiply => 0x55,
        KeyCode::NumpadSubtract => 0x56,
        KeyCode::NumpadAdd => 0x57,
        KeyCode::NumpadEnter => 0x58,
        KeyCode::Numpad1 => 0x59,
        KeyCode::Numpad2 => 0x5A,
        KeyCode::Numpad3 => 0x5B,
        KeyCode::Numpad4 => 0x5C,
        KeyCode::Numpad5 => 0x5D,
        KeyCode::Numpad6 => 0x5E,
        KeyCode::Numpad7 => 0x5F,
        KeyCode::Numpad8 => 0x60,
        KeyCode::Numpad9 => 0x61,
        KeyCode::Numpad0 => 0x62,
        KeyCode::NumpadDecimal => 0x63,
        // Modifiers
        KeyCode::ControlLeft => 0xE0,
        KeyCode::ShiftLeft => 0xE1,
        KeyCode::AltLeft => 0xE2,
        KeyCode::SuperLeft => 0xE3,
        KeyCode::ControlRight => 0xE4,
        KeyCode::ShiftRight => 0xE5,
        KeyCode::AltRight => 0xE6,
        KeyCode::SuperRight => 0xE7,
        _ => 0,
    }
}

/// Parse a `[capture] toggle = "..."` string into a [`PhysicalKey`]. Accepts
/// either a short alias (`"RightCtrl"`, `"LeftCtrl"`, `"RightAlt"`, …) or the
/// exact name of a winit [`KeyCode`] variant (e.g. `"F12"`, `"ScrollLock"`).
/// Returns `None` for unknown names so the caller can fall back to the
/// default and log a warning.
pub fn parse_toggle_key(name: &str) -> Option<PhysicalKey> {
    let code = match name {
        // Aliases for the modifier keys, which are the natural toggles.
        "RightCtrl" | "RCtrl" | "ControlRight" => KeyCode::ControlRight,
        "LeftCtrl" | "LCtrl" | "ControlLeft" => KeyCode::ControlLeft,
        "RightAlt" | "RAlt" | "AltRight" => KeyCode::AltRight,
        "LeftAlt" | "LAlt" | "AltLeft" => KeyCode::AltLeft,
        "RightShift" | "RShift" | "ShiftRight" => KeyCode::ShiftRight,
        "LeftShift" | "LShift" | "ShiftLeft" => KeyCode::ShiftLeft,
        "RightSuper" | "RGui" | "SuperRight" => KeyCode::SuperRight,
        "LeftSuper" | "LGui" | "SuperLeft" => KeyCode::SuperLeft,
        // F-row, plenty of non-collision options.
        "F1" => KeyCode::F1,
        "F2" => KeyCode::F2,
        "F3" => KeyCode::F3,
        "F4" => KeyCode::F4,
        "F5" => KeyCode::F5,
        "F6" => KeyCode::F6,
        "F7" => KeyCode::F7,
        "F8" => KeyCode::F8,
        "F9" => KeyCode::F9,
        "F10" => KeyCode::F10,
        "F11" => KeyCode::F11,
        "F12" => KeyCode::F12,
        "ScrollLock" => KeyCode::ScrollLock,
        "Pause" => KeyCode::Pause,
        "CapsLock" => KeyCode::CapsLock,
        _ => return None,
    };
    Some(PhysicalKey::Code(code))
}

/// Map a winit mouse button to its forwarded counterpart. Returns `None` for
/// `Back`/`Forward`/`Other(_)`, which the pico-keeb plugin doesn't model.
pub fn map_mouse_button(button: WinitMouseButton) -> Option<CoreMouseButton> {
    match button {
        WinitMouseButton::Left => Some(CoreMouseButton::Left),
        WinitMouseButton::Right => Some(CoreMouseButton::Right),
        WinitMouseButton::Middle => Some(CoreMouseButton::Middle),
        _ => None,
    }
}

/// Approximate pixel-scroll deltas as line-deltas. `MouseWheel` events from
/// most platforms arrive as `LineDelta` already; touchpads emit `PixelDelta`
/// in physical pixels, which we coarsen at ~20 px per line so plugins see a
/// single scroll unit regardless of source.
pub const PIXELS_PER_SCROLL_LINE: f32 = 20.0;

/// Should the host flip `capture_mode` for this keyboard event? Pure
/// function so the "press-edge only, no double-toggle on repeat or release"
/// guarantee is testable without driving winit. Returns true only on the
/// initial press of the configured toggle key.
pub fn should_flip_capture_mode(
    physical_key: PhysicalKey,
    toggle_key: PhysicalKey,
    pressed: bool,
    repeat: bool,
) -> bool {
    physical_key == toggle_key && pressed && !repeat
}

/// Build the [`AppEvent::KeyboardInput`] the host should forward to plugins,
/// or `None` if this event should be suppressed (capture off, or the key is
/// the toggle key). Shared between the app's hot path and the integration
/// tests so the two can't drift.
pub fn keyboard_event_for_plugins(
    capture_mode: bool,
    physical_key: PhysicalKey,
    toggle_key: PhysicalKey,
    modifiers: ModifiersState,
    pressed: bool,
    repeat: bool,
) -> Option<AppEvent> {
    if !capture_mode || physical_key == toggle_key {
        return None;
    }
    let mut modifiers = modifiers_state_to_pico_keeb_mask(modifiers);
    if let Some(bit) = physical_key_modifier_bit(physical_key) {
        if pressed {
            modifiers |= bit;
        } else {
            modifiers &= !bit;
        }
    }
    let key_code = physical_key_to_hid_usage(physical_key);
    let scan_code = {
        use winit::platform::scancode::PhysicalKeyExtScancode;
        physical_key.to_scancode().unwrap_or(0)
    };
    Some(AppEvent::KeyboardInput {
        key_code,
        scan_code,
        modifiers,
        pressed,
        repeat,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modifiers_empty_maps_to_zero() {
        assert_eq!(
            modifiers_state_to_pico_keeb_mask(ModifiersState::empty()),
            0
        );
    }

    #[test]
    fn modifiers_lone_shift_maps_to_lshift() {
        assert_eq!(
            modifiers_state_to_pico_keeb_mask(ModifiersState::SHIFT),
            MOD_LSHIFT
        );
    }

    #[test]
    fn modifiers_lone_gui_maps_to_lgui() {
        assert_eq!(
            modifiers_state_to_pico_keeb_mask(ModifiersState::SUPER),
            MOD_LGUI
        );
    }

    #[test]
    fn modifiers_both_sides_modifiers_all_set() {
        // "Both-sides modifiers" — all four high-level bits held simultaneously
        // (e.g. Ctrl+Shift+Alt+Super). Each maps to its left-side pico-keeb bit.
        let state = ModifiersState::CONTROL
            | ModifiersState::SHIFT
            | ModifiersState::ALT
            | ModifiersState::SUPER;
        assert_eq!(
            modifiers_state_to_pico_keeb_mask(state),
            MOD_LCTRL | MOD_LSHIFT | MOD_LALT | MOD_LGUI
        );
    }

    #[test]
    fn modifiers_ctrl_shift_maps_to_lctrl_lshift() {
        let state = ModifiersState::CONTROL | ModifiersState::SHIFT;
        assert_eq!(
            modifiers_state_to_pico_keeb_mask(state),
            MOD_LCTRL | MOD_LSHIFT
        );
    }

    #[test]
    fn physical_key_modifier_bit_recovers_side() {
        assert_eq!(
            physical_key_modifier_bit(PhysicalKey::Code(KeyCode::ShiftRight)),
            Some(MOD_RSHIFT)
        );
        assert_eq!(
            physical_key_modifier_bit(PhysicalKey::Code(KeyCode::ControlLeft)),
            Some(MOD_LCTRL)
        );
        assert_eq!(
            physical_key_modifier_bit(PhysicalKey::Code(KeyCode::KeyA)),
            None
        );
    }

    #[test]
    fn parse_toggle_key_known_aliases() {
        assert_eq!(
            parse_toggle_key("RightCtrl"),
            Some(PhysicalKey::Code(KeyCode::ControlRight))
        );
        assert_eq!(
            parse_toggle_key("ScrollLock"),
            Some(PhysicalKey::Code(KeyCode::ScrollLock))
        );
        assert_eq!(
            parse_toggle_key("F12"),
            Some(PhysicalKey::Code(KeyCode::F12))
        );
        assert_eq!(parse_toggle_key("not-a-real-key"), None);
    }

    #[test]
    fn hid_usage_covers_common_keys() {
        assert_eq!(
            physical_key_to_hid_usage(PhysicalKey::Code(KeyCode::KeyA)),
            0x04
        );
        assert_eq!(
            physical_key_to_hid_usage(PhysicalKey::Code(KeyCode::Enter)),
            0x28
        );
        assert_eq!(
            physical_key_to_hid_usage(PhysicalKey::Code(KeyCode::ControlRight)),
            0xE4
        );
        // Unrecognised → 0 sentinel.
        assert_eq!(
            physical_key_to_hid_usage(PhysicalKey::Unidentified(
                winit::keyboard::NativeKeyCode::Unidentified
            )),
            0
        );
    }

    #[test]
    fn toggle_key_flips_once_per_press() {
        // Drive a press / repeat-press / release / second press sequence,
        // tracking how many times the predicate fires. Exactly two flips
        // expected: the initial press and the second press, with the
        // intervening repeat and release contributing nothing.
        let toggle = PhysicalKey::Code(KeyCode::ControlRight);
        let sequence = [
            (toggle, true, false),  // first press
            (toggle, true, true),   // OS auto-repeat
            (toggle, true, true),   // more repeats
            (toggle, false, false), // release
            (toggle, true, false),  // press again
        ];
        let flips = sequence
            .iter()
            .filter(|(k, pressed, repeat)| should_flip_capture_mode(*k, toggle, *pressed, *repeat))
            .count();
        assert_eq!(flips, 2);
    }

    #[test]
    fn toggle_key_ignores_other_keys() {
        let toggle = PhysicalKey::Code(KeyCode::ControlRight);
        let other = PhysicalKey::Code(KeyCode::KeyA);
        assert!(!should_flip_capture_mode(other, toggle, true, false));
        assert!(!should_flip_capture_mode(other, toggle, false, false));
    }

    #[test]
    fn map_mouse_button_filters_extras() {
        assert_eq!(
            map_mouse_button(WinitMouseButton::Left),
            Some(CoreMouseButton::Left)
        );
        assert_eq!(
            map_mouse_button(WinitMouseButton::Right),
            Some(CoreMouseButton::Right)
        );
        assert_eq!(
            map_mouse_button(WinitMouseButton::Middle),
            Some(CoreMouseButton::Middle)
        );
        assert_eq!(map_mouse_button(WinitMouseButton::Other(7)), None);
    }
}
