//! Turn key presses into a profile name (the overlay has no real text field).
//! Only letters, digits, '-' and '_' are accepted.

use crate::keys::Key;

pub const MAX_LEN: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextAction {
    Confirm,
    Cancel,
}

pub fn is_confirm(k: Key) -> bool {
    matches!(k, Key::RETURN | Key::KP_ENTER)
}

/// The character `key` types in a profile name, or None if it types nothing there.
pub fn key_to_char(key: Key, shift: bool) -> Option<char> {
    use Key::*;
    let letter = match key {
        A => 'a',
        B => 'b',
        C => 'c',
        D => 'd',
        E => 'e',
        F => 'f',
        G => 'g',
        H => 'h',
        I => 'i',
        J => 'j',
        K => 'k',
        L => 'l',
        M => 'm',
        N => 'n',
        O => 'o',
        P => 'p',
        Q => 'q',
        R => 'r',
        S => 's',
        T => 't',
        U => 'u',
        V => 'v',
        W => 'w',
        X => 'x',
        Y => 'y',
        Z => 'z',
        _ => ' ',
    };
    if letter != ' ' {
        return Some(if shift { letter.to_ascii_uppercase() } else { letter });
    }
    let digit = match key {
        KEY_0 | KP_0 => '0',
        KEY_1 | KP_1 => '1',
        KEY_2 | KP_2 => '2',
        KEY_3 | KP_3 => '3',
        KEY_4 | KP_4 => '4',
        KEY_5 | KP_5 => '5',
        KEY_6 | KP_6 => '6',
        KEY_7 | KP_7 => '7',
        KEY_8 | KP_8 => '8',
        KEY_9 | KP_9 => '9',
        _ => ' ',
    };
    if digit != ' ' {
        return if shift { None } else { Some(digit) };
    } // shifted digits are symbols: not allowed
    if key == MINUS {
        return Some(if shift { '_' } else { '-' });
    }
    None
}

/// Edit `text` with one key press. Returns the new text and an optional confirm/cancel action.
pub fn apply_key(text: &str, key: Key, shift: bool) -> (String, Option<TextAction>) {
    if is_confirm(key) {
        return (text.to_string(), Some(TextAction::Confirm));
    }
    if key == Key::ESCAPE {
        return (text.to_string(), Some(TextAction::Cancel));
    }
    if key == Key::BACKSPACE {
        let mut t = text.to_string();
        t.pop();
        return (t, None);
    }
    match key_to_char(key, shift) {
        Some(c) if text.chars().count() < MAX_LEN => (format!("{text}{c}"), None),
        _ => (text.to_string(), None),
    }
}
