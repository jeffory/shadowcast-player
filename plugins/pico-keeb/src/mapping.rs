//! Translation helpers between host-side `AppEvent::KeyboardInput` /
//! `MouseMotion` / `MouseScroll` values and the pico-keeb-protocol HID byte
//! representation.
//!
//! Almost all of the heavy lifting (winit `KeyCode` → USB HID Usage on page
//! `0x07`) already happens host-side in `shadowcast-player/src/input.rs`. The
//! plugin receives a `key_code: u32` that is *already* a HID usage. This
//! module's job is only to:
//!
//! - validate that the host-supplied HID usage is something we want to track
//!   in the 6KRO `held_keys` slot table (filtering out the `0` sentinel and
//!   the modifier-byte range, which we already handle via the `modifiers`
//!   bitmask on the same event),
//! - clamp f32 mouse deltas to the protocol's `i8` per-frame range, and
//! - expose a tiny helper for mapping `shadowcast_core::MouseButton` to the
//!   pico-keeb mouse-buttons bitmask.

use shadowcast_core::MouseButton;

/// Validate a host-supplied USB HID Usage on the Keyboard / Keypad page
/// (`0x07`) and return the single byte that belongs in `Frame::Kbd.keys`,
/// or `None` if this event should not be tracked in the held-key slots.
///
/// `None` is returned for:
/// - `0` — the host's "unknown / unmapped key" sentinel.
/// - `0xE0..=0xE7` — modifier keys; these are handled via the `modifiers`
///   bitmask on the same `AppEvent::KeyboardInput`, not via the 6KRO slots.
/// - anything `> 0xFF` — outside the Keyboard/Keypad usage range. The host
///   today never emits such values, but this keeps the function total.
pub fn hid_usage_to_key_byte(key_code: u32) -> Option<u8> {
    if key_code == 0 || key_code > 0xFF {
        return None;
    }
    let byte = key_code as u8;
    if (0xE0..=0xE7).contains(&byte) {
        return None;
    }
    Some(byte)
}

/// Map a `shadowcast_core::MouseButton` to its pico-keeb mouse-button bit.
/// The pico-keeb USB HID mouse report uses the standard Boot Mouse bitmask:
/// bit 0 = Left, bit 1 = Right, bit 2 = Middle.
pub const fn mouse_button_bit(button: MouseButton) -> u8 {
    match button {
        MouseButton::Left => 0x01,
        MouseButton::Right => 0x02,
        MouseButton::Middle => 0x04,
    }
}

/// Quantise an accumulated f32 delta into an i8 step + the residual. The
/// pico-keeb binary mouse frame carries one i8 per axis per frame, so a
/// motion of e.g. `dx = 200.0` becomes two frames (127 + 73). Sub-pixel
/// motion is carried over via the returned residual so we don't drop it.
pub fn split_i8(value: f32) -> (i8, f32) {
    if !value.is_finite() {
        return (0, 0.0);
    }
    let clamped = value.clamp(i8::MIN as f32, i8::MAX as f32);
    let step = clamped.trunc() as i8;
    (step, value - step as f32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_keycodes_pass_through() {
        // Round-trip a sample of HID usages from the keyboard/keypad page —
        // the host puts these directly into AppEvent::KeyboardInput.key_code.
        assert_eq!(hid_usage_to_key_byte(0x04), Some(0x04)); // A
        assert_eq!(hid_usage_to_key_byte(0x28), Some(0x28)); // Enter
        assert_eq!(hid_usage_to_key_byte(0x29), Some(0x29)); // Esc
        assert_eq!(hid_usage_to_key_byte(0x2C), Some(0x2C)); // Space
        assert_eq!(hid_usage_to_key_byte(0x3A), Some(0x3A)); // F1
        assert_eq!(hid_usage_to_key_byte(0x52), Some(0x52)); // Up arrow
    }

    #[test]
    fn unknown_keycode_returns_none() {
        assert_eq!(hid_usage_to_key_byte(0), None);
    }

    #[test]
    fn modifier_range_returns_none() {
        // 0xE0..=0xE7 are the modifier-key usages — Ctrl/Shift/Alt/GUI
        // (left and right). They are reported as held in
        // AppEvent::KeyboardInput.modifiers, not in the 6KRO slots.
        for u in 0xE0u32..=0xE7 {
            assert_eq!(hid_usage_to_key_byte(u), None, "0x{:02X}", u);
        }
    }

    #[test]
    fn out_of_byte_range_returns_none() {
        assert_eq!(hid_usage_to_key_byte(0x100), None);
        assert_eq!(hid_usage_to_key_byte(u32::MAX), None);
    }

    #[test]
    fn split_i8_basic() {
        assert_eq!(split_i8(0.0), (0, 0.0));
        assert_eq!(split_i8(5.0), (5, 0.0));
        assert_eq!(split_i8(-5.0), (-5, 0.0));
    }

    #[test]
    fn split_i8_preserves_fractional() {
        let (step, residual) = split_i8(3.4);
        assert_eq!(step, 3);
        assert!((residual - 0.4).abs() < 1e-5);

        let (step, residual) = split_i8(-2.7);
        assert_eq!(step, -2);
        assert!((residual - -0.7).abs() < 1e-5);
    }

    #[test]
    fn split_i8_clamps_large() {
        // 200.0 saturates this frame at i8::MAX; the leftover 73 carries
        // over to the next frame via the returned residual.
        let (step, residual) = split_i8(200.0);
        assert_eq!(step, i8::MAX);
        assert!((residual - (200.0 - i8::MAX as f32)).abs() < 1e-3);

        let (step, residual) = split_i8(-200.0);
        assert_eq!(step, i8::MIN);
        assert!((residual - (-200.0 - i8::MIN as f32)).abs() < 1e-3);
    }

    #[test]
    fn split_i8_handles_nonfinite() {
        assert_eq!(split_i8(f32::NAN), (0, 0.0));
        assert_eq!(split_i8(f32::INFINITY), (0, 0.0));
        assert_eq!(split_i8(f32::NEG_INFINITY), (0, 0.0));
    }

    #[test]
    fn mouse_button_bits() {
        assert_eq!(mouse_button_bit(MouseButton::Left), 0x01);
        assert_eq!(mouse_button_bit(MouseButton::Right), 0x02);
        assert_eq!(mouse_button_bit(MouseButton::Middle), 0x04);
    }
}
