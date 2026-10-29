//! Every key and mouse input on a Mac. Key values are macOS virtual key codes (Carbon `kVK_*`).

macro_rules! keys {
    ($($n:ident = $v:expr),* $(,)?) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        #[repr(u16)]
        pub enum Key { $($n = $v),* }
        impl Key {
            pub const ALL: &'static [Key] = &[$(Key::$n),*];
            pub fn name(self) -> &'static str { match self { $(Key::$n => stringify!($n)),* } }
            pub fn vk(self) -> u16 { self as u16 }
        }
    };
}

keys! {
    A=0x00,S=0x01,D=0x02,F=0x03,H=0x04,G=0x05,Z=0x06,X=0x07,C=0x08,V=0x09,ISO_SECTION=0x0A,B=0x0B,Q=0x0C,W=0x0D,E=0x0E,R=0x0F,
    Y=0x10,T=0x11,KEY_1=0x12,KEY_2=0x13,KEY_3=0x14,KEY_4=0x15,KEY_6=0x16,KEY_5=0x17,EQUAL=0x18,KEY_9=0x19,KEY_7=0x1A,MINUS=0x1B,
    KEY_8=0x1C,KEY_0=0x1D,RIGHT_BRACKET=0x1E,O=0x1F,U=0x20,LEFT_BRACKET=0x21,I=0x22,P=0x23,RETURN=0x24,L=0x25,J=0x26,QUOTE=0x27,
    K=0x28,SEMICOLON=0x29,BACKSLASH=0x2A,COMMA=0x2B,SLASH=0x2C,N=0x2D,M=0x2E,PERIOD=0x2F,TAB=0x30,SPACE=0x31,GRAVE=0x32,
    BACKSPACE=0x33,ESCAPE=0x35,RIGHT_COMMAND=0x36,LEFT_COMMAND=0x37,LEFT_SHIFT=0x38,CAPS_LOCK=0x39,LEFT_OPTION=0x3A,
    LEFT_CONTROL=0x3B,RIGHT_SHIFT=0x3C,RIGHT_OPTION=0x3D,RIGHT_CONTROL=0x3E,FN=0x3F,F17=0x40,KP_DECIMAL=0x41,KP_MULTIPLY=0x43,
    KP_PLUS=0x45,KP_CLEAR=0x47,JIS_YEN=0x5D,JIS_UNDERSCORE=0x5E,JIS_KP_COMMA=0x5F,JIS_EISU=0x66,JIS_KANA=0x68,VOLUME_UP=0x48,
    VOLUME_DOWN=0x49,VOLUME_MUTE=0x4A,KP_DIVIDE=0x4B,KP_ENTER=0x4C,KP_MINUS=0x4E,F18=0x4F,F19=0x50,KP_EQUALS=0x51,KP_0=0x52,
    KP_1=0x53,KP_2=0x54,KP_3=0x55,KP_4=0x56,KP_5=0x57,KP_6=0x58,KP_7=0x59,F20=0x5A,KP_8=0x5B,KP_9=0x5C,F5=0x60,F6=0x61,F7=0x62,
    F3=0x63,F8=0x64,F9=0x65,F11=0x67,F13=0x69,F16=0x6A,F14=0x6B,F10=0x6D,F12=0x6F,F15=0x71,HELP=0x72,HOME=0x73,PAGE_UP=0x74,
    FORWARD_DELETE=0x75,F4=0x76,END=0x77,F2=0x78,PAGE_DOWN=0x79,F1=0x7A,LEFT_ARROW=0x7B,RIGHT_ARROW=0x7C,DOWN_ARROW=0x7D,UP_ARROW=0x7E,
}

impl Key {
    pub fn from_vk(vk: u16) -> Option<Key> {
        Key::ALL.iter().copied().find(|k| k.vk() == vk)
    }
    pub fn is_modifier(self) -> bool {
        use Key::*;
        matches!(
            self,
            LEFT_SHIFT
                | RIGHT_SHIFT
                | LEFT_CONTROL
                | RIGHT_CONTROL
                | LEFT_OPTION
                | RIGHT_OPTION
                | LEFT_COMMAND
                | RIGHT_COMMAND
                | CAPS_LOCK
                | FN
        )
    }
}

macro_rules! mouse {
    ($($n:ident),* $(,)?) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum MouseInput { $($n),* }
        impl MouseInput {
            pub const ALL: &'static [MouseInput] = &[$(MouseInput::$n),*];
            pub fn name(self) -> &'static str { match self { $(MouseInput::$n => stringify!($n)),* } }
        }
    };
}
mouse!(
    MOUSE_LEFT,
    MOUSE_RIGHT,
    MOUSE_MIDDLE,
    MOUSE_BUTTON4,
    MOUSE_BUTTON5,
    SCROLL_UP,
    SCROLL_DOWN,
    SCROLL_LEFT,
    SCROLL_RIGHT,
    MOUSE_MOVE_UP,
    MOUSE_MOVE_DOWN,
    MOUSE_MOVE_LEFT,
    MOUSE_MOVE_RIGHT
);

/// A key or a mouse input. Ordered: keys before mouse inputs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Input {
    Key(Key),
    Mouse(MouseInput),
}

impl Input {
    pub fn name(self) -> &'static str {
        match self {
            Input::Key(k) => k.name(),
            Input::Mouse(m) => m.name(),
        }
    }
}
impl From<Key> for Input {
    fn from(k: Key) -> Self {
        Input::Key(k)
    }
}
impl From<MouseInput> for Input {
    fn from(m: MouseInput) -> Self {
        Input::Mouse(m)
    }
}

/// Parse "w" / "LEFT_SHIFT" / "mouse_left"; a bare digit means KEY_<digit>.
pub fn parse_input(name: &str) -> Result<Input, String> {
    let mut t = name.trim().to_uppercase();
    if t.len() == 1 && t.chars().all(|c| c.is_ascii_digit()) {
        t = format!("KEY_{t}");
    }
    if let Some(k) = Key::ALL.iter().find(|k| k.name() == t) {
        return Ok(Input::Key(*k));
    }
    if let Some(m) = MouseInput::ALL.iter().find(|m| m.name() == t) {
        return Ok(Input::Mouse(*m));
    }
    Err(format!("unknown key/mouse input {name:?}"))
}

pub type Chord = std::collections::BTreeSet<Input>;

/// Parse "LEFT_SHIFT+W".
pub fn parse_chord(text: &str) -> Result<Chord, String> {
    let parts: Vec<&str> = text.split('+').collect();
    if text.trim().is_empty() || parts.iter().any(|p| p.trim().is_empty()) {
        return Err(format!("malformed chord {text:?}: use names joined by single '+', e.g. LEFT_SHIFT+W"));
    }
    parts.iter().map(|p| parse_input(p)).collect()
}

/// Stable text form: modifiers first, then other keys, then mouse inputs (each group by name).
pub fn chord_to_str(chord: &Chord) -> String {
    let mut v: Vec<&Input> = chord.iter().collect();
    v.sort_by_key(|i| match i {
        Input::Key(k) => (if k.is_modifier() { 0 } else { 1 }, k.name()),
        Input::Mouse(m) => (2, m.name()),
    });
    v.iter().map(|i| i.name()).collect::<Vec<_>>().join("+")
}
