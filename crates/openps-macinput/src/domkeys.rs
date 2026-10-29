//! Translate DOM `KeyboardEvent.code` / `MouseEvent.button` to the engine's macOS key and mouse inputs.
//! `code` names the physical key (layout independent), which is what a game mapping wants.

use openps_input::keys::{Key, MouseInput};

pub fn key_from_dom_code(code: &str) -> Option<Key> {
    use Key::*;
    Some(match code {
        "KeyA" => A,
        "KeyB" => B,
        "KeyC" => C,
        "KeyD" => D,
        "KeyE" => E,
        "KeyF" => F,
        "KeyG" => G,
        "KeyH" => H,
        "KeyI" => I,
        "KeyJ" => J,
        "KeyK" => K,
        "KeyL" => L,
        "KeyM" => M,
        "KeyN" => N,
        "KeyO" => O,
        "KeyP" => P,
        "KeyQ" => Q,
        "KeyR" => R,
        "KeyS" => S,
        "KeyT" => T,
        "KeyU" => U,
        "KeyV" => V,
        "KeyW" => W,
        "KeyX" => X,
        "KeyY" => Y,
        "KeyZ" => Z,
        "Digit0" => KEY_0,
        "Digit1" => KEY_1,
        "Digit2" => KEY_2,
        "Digit3" => KEY_3,
        "Digit4" => KEY_4,
        "Digit5" => KEY_5,
        "Digit6" => KEY_6,
        "Digit7" => KEY_7,
        "Digit8" => KEY_8,
        "Digit9" => KEY_9,
        "F1" => F1,
        "F2" => F2,
        "F3" => F3,
        "F4" => F4,
        "F5" => F5,
        "F6" => F6,
        "F7" => F7,
        "F8" => F8,
        "F9" => F9,
        "F10" => F10,
        "F11" => F11,
        "F12" => F12,
        "F13" => F13,
        "F14" => F14,
        "F15" => F15,
        "F16" => F16,
        "F17" => F17,
        "F18" => F18,
        "F19" => F19,
        "F20" => F20,
        "ArrowUp" => UP_ARROW,
        "ArrowDown" => DOWN_ARROW,
        "ArrowLeft" => LEFT_ARROW,
        "ArrowRight" => RIGHT_ARROW,
        "Space" => SPACE,
        "Enter" => RETURN,
        "Tab" => TAB,
        "Escape" => ESCAPE,
        "Backspace" => BACKSPACE,
        "Delete" => FORWARD_DELETE,
        "Home" => HOME,
        "End" => END,
        "PageUp" => PAGE_UP,
        "PageDown" => PAGE_DOWN,
        "Insert" => HELP,
        "CapsLock" => CAPS_LOCK,
        "ShiftLeft" => LEFT_SHIFT,
        "ShiftRight" => RIGHT_SHIFT,
        "ControlLeft" => LEFT_CONTROL,
        "ControlRight" => RIGHT_CONTROL,
        "AltLeft" => LEFT_OPTION,
        "AltRight" => RIGHT_OPTION,
        "MetaLeft" => LEFT_COMMAND,
        "MetaRight" => RIGHT_COMMAND,
        "Minus" => MINUS,
        "Equal" => EQUAL,
        "BracketLeft" => LEFT_BRACKET,
        "BracketRight" => RIGHT_BRACKET,
        "Backslash" => BACKSLASH,
        "Semicolon" => SEMICOLON,
        "Quote" => QUOTE,
        "Comma" => COMMA,
        "Period" => PERIOD,
        "Slash" => SLASH,
        "Backquote" => GRAVE,
        "IntlBackslash" => ISO_SECTION,
        "IntlYen" => JIS_YEN,
        "IntlRo" => JIS_UNDERSCORE,
        "Numpad0" => KP_0,
        "Numpad1" => KP_1,
        "Numpad2" => KP_2,
        "Numpad3" => KP_3,
        "Numpad4" => KP_4,
        "Numpad5" => KP_5,
        "Numpad6" => KP_6,
        "Numpad7" => KP_7,
        "Numpad8" => KP_8,
        "Numpad9" => KP_9,
        "NumpadDecimal" => KP_DECIMAL,
        "NumpadMultiply" => KP_MULTIPLY,
        "NumpadAdd" => KP_PLUS,
        "NumpadSubtract" => KP_MINUS,
        "NumpadDivide" => KP_DIVIDE,
        "NumpadEnter" => KP_ENTER,
        "NumpadEqual" => KP_EQUALS,
        "NumLock" => KP_CLEAR,
        _ => return None,
    })
}

/// DOM `button`: 0 left, 1 middle, 2 right, 3 back, 4 forward.
pub fn mouse_from_button(b: i32) -> Option<MouseInput> {
    Some(match b {
        0 => MouseInput::MOUSE_LEFT,
        1 => MouseInput::MOUSE_MIDDLE,
        2 => MouseInput::MOUSE_RIGHT,
        3 => MouseInput::MOUSE_BUTTON4,
        4 => MouseInput::MOUSE_BUTTON5,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letters_digits_and_function_keys_map_to_the_right_virtual_keys() {
        assert_eq!(key_from_dom_code("KeyW"), Some(Key::W));
        assert_eq!(key_from_dom_code("KeyW").unwrap().vk(), 0x0D);
        assert_eq!(key_from_dom_code("Digit1").unwrap().vk(), 0x12);
        assert_eq!(key_from_dom_code("F10").unwrap().vk(), 0x6D);
        assert_eq!(key_from_dom_code("Space").unwrap().vk(), 0x31);
        assert_eq!(key_from_dom_code("MetaLeft"), Some(Key::LEFT_COMMAND));
        assert_eq!(key_from_dom_code("AltLeft"), Some(Key::LEFT_OPTION));
    }

    #[test]
    fn every_letter_digit_and_f_key_is_covered_and_unique() {
        let mut seen = std::collections::HashSet::new();
        for c in ('A'..='Z')
            .map(|c| format!("Key{c}"))
            .chain((0..=9).map(|d| format!("Digit{d}")))
            .chain((1..=20).map(|n| format!("F{n}")))
        {
            let k = key_from_dom_code(&c).unwrap_or_else(|| panic!("{c} unmapped"));
            assert!(seen.insert(k), "{c} maps to a key another code already uses");
        }
    }

    #[test]
    fn unknown_and_hostile_codes_are_ignored() {
        for c in ["", "Unidentified", "keyw", "KeyW\0", "Key", &"K".repeat(10_000), "MediaPlayPause", "\u{202e}KeyA"] {
            assert_eq!(key_from_dom_code(c), None, "{c:?}");
        }
        assert_eq!(mouse_from_button(-1), None);
        assert_eq!(mouse_from_button(5), None);
        assert_eq!(mouse_from_button(i32::MAX), None);
        assert_eq!(mouse_from_button(0), Some(MouseInput::MOUSE_LEFT));
    }

    #[test]
    fn every_engine_key_a_dom_code_can_produce_is_a_real_key() {
        // each mapped key is one the engine knows by its virtual-key code (round trip)
        for code in ["KeyA", "F1", "ArrowUp", "Numpad5", "IntlBackslash", "Backquote", "Delete"] {
            let k = key_from_dom_code(code).unwrap();
            assert_eq!(Key::from_vk(k.vk()), Some(k), "{code}");
        }
    }
}
