//! Control channel frames over the kept-alive TCP connection (spec 02 section 4.4).
//!
//! Frame = 8-byte header (`len:u32 BE`, `type:u16 BE`, `00 00`) + payload encrypted with the session cipher using
//! a per-direction counter. A frame with an empty payload is not encrypted and consumes no counter. Which
//! inbound frames consume a counter is assumed to be "every non-empty one" (UNCERTAIN; the app also re-anchors
//! its inbound counter within +-32 when a pad index looks wrong, which [`CtrlRx`] reproduces).

use openps_crypto::RpCipher;

pub const HEADER_LEN: usize = 8;
const MAX_PAYLOAD: usize = 64 * 1024;

pub mod ty {
    pub const DISPLAY_STATUS: u16 = 1;
    pub const PIN_REQUIRED: u16 = 4;
    pub const PIN_RESULT: u16 = 5;
    pub const PAD_DROPPED: u16 = 10;
    pub const STREAMABLE: u16 = 22;
    pub const SESSION_ID: u16 = 51;
    pub const IDENTITY: u16 = 97;
    pub const PASSCODE_REQUEST: u16 = 100;
    pub const KEEPALIVE: u16 = 254;
    pub const PAD_JOIN_RESULT: u16 = 32776;
    pub const PAD_LEAVE_RESULT: u16 = 32777;
    pub const USER_JOIN_RESULT: u16 = 32872;
    // outbound
    pub const PIN: u16 = 0x8004;
    pub const MIC_REQUEST: u16 = 0x0030;
    pub const MIC_STATUS: u16 = 0x0036;
    pub const REST_MODE: u16 = 0x0050;
    pub const PAD_JOIN: u16 = 0x0008;
    pub const PAD_LEAVE: u16 = 0x0009;
}

/// Reply to a keepalive: raw, unencrypted.
pub const KEEPALIVE_REPLY: [u8; 8] = [0, 0, 0, 0, 0x01, 0xFE, 0, 0];

/// Outbound frames with their payload encrypted at the next counter value.
pub struct CtrlTx {
    cipher: RpCipher,
    counter: u32,
}

impl CtrlTx {
    pub fn new(cipher: RpCipher, counter: u32) -> Self {
        Self { cipher, counter }
    }

    pub fn counter(&self) -> u32 {
        self.counter
    }

    pub fn frame(&mut self, ty: u16, payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(HEADER_LEN + payload.len());
        out.extend((payload.len() as u32).to_be_bytes());
        out.extend(ty.to_be_bytes());
        out.extend([0, 0]);
        let mut p = payload.to_vec();
        if !p.is_empty() {
            let Some(next) = self.counter.checked_add(1) else {
                // never wrap: that would reuse keystream. The session is unusable; send nothing.
                return Vec::new();
            };
            self.cipher.encrypt(&mut p, self.counter);
            self.counter = next;
        }
        out.extend(p);
        out
    }

    pub fn pin(&mut self, pin: &str) -> Vec<u8> {
        self.frame(ty::PIN, pin.as_bytes())
    }
    pub fn mic_request(&mut self) -> Vec<u8> {
        self.frame(ty::MIC_REQUEST, &[0, 0])
    }
    /// Microphone status; `muted = true` sets the mute flag.
    pub fn mic_status(&mut self, muted: bool) -> Vec<u8> {
        self.frame(ty::MIC_STATUS, &[0, 1, muted as u8, 0x59])
    }
    pub fn rest_mode(&mut self) -> Vec<u8> {
        self.frame(ty::REST_MODE, &[])
    }
    /// Join code for a client controller type: 3 -> 1, 1 -> 2, anything else -> 3 (the client's type 6 sends 3).
    pub fn pad_join(&mut self, pad: u8, controller_type: u8) -> Vec<u8> {
        let code = match controller_type {
            3 => 1,
            1 => 2,
            _ => 3,
        };
        self.frame(ty::PAD_JOIN, &[pad, code])
    }
    pub fn pad_leave(&mut self, pad: u8) -> Vec<u8> {
        self.frame(ty::PAD_LEAVE, &[pad])
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Inbound {
    /// type 1: false = content cannot be displayed.
    DisplayStatus {
        can_display: bool,
    },
    PinRequired,
    /// type 5: 1 = invalid PIN, 2 = login not possible.
    PinResult(u8),
    PadDropped(u8),
    Streamable(bool),
    SessionId(String),
    PadJoinResult {
        pad: u8,
        accepted: bool,
    },
    PadLeaveResult {
        pad: u8,
        accepted: bool,
    },
    /// type 100: the console wants the pad's 4-digit passcode.
    PasscodeRequest(u8),
    /// type 32872: 0 = seated, 3 = passcode required, else refused.
    UserJoinResult {
        pad: u8,
        status: u8,
    },
    /// type 97 with a 24-byte payload: the console cannot map the pad's account to a console user.
    AccountUnknown,
    Keepalive,
    Unknown {
        ty: u16,
        len: usize,
    },
}

impl Inbound {
    /// The pad index this message names, if any (used to detect an out-of-step keystream).
    fn pad(&self) -> Option<u8> {
        match self {
            Inbound::PadDropped(p) | Inbound::PasscodeRequest(p) => Some(*p),
            Inbound::PadJoinResult { pad, .. }
            | Inbound::PadLeaveResult { pad, .. }
            | Inbound::UserJoinResult { pad, .. } => Some(*pad),
            _ => None,
        }
    }

    /// False if this looks like decryption with the wrong counter (a pad index >= 4).
    pub fn plausible(&self) -> bool {
        self.pad().is_none_or(|p| p < 4)
    }

    pub fn parse(ty: u16, p: &[u8]) -> Inbound {
        let b = |i: usize| p.get(i).copied();
        match ty {
            ty::DISPLAY_STATUS => Inbound::DisplayStatus { can_display: b(0) == Some(0) },
            ty::PIN_REQUIRED => Inbound::PinRequired,
            ty::PIN_RESULT => Inbound::PinResult(b(0).unwrap_or(0)),
            ty::PAD_DROPPED if b(0).is_some() => Inbound::PadDropped(p[0]),
            ty::STREAMABLE => Inbound::Streamable(!(b(0) == Some(1) && b(1) == Some(0xFF))),
            ty::SESSION_ID => Inbound::SessionId(String::from_utf8_lossy(p).trim_end_matches('\0').to_string()),
            ty::PAD_JOIN_RESULT if p.len() >= 2 => Inbound::PadJoinResult { pad: p[0], accepted: p[1] == 0 },
            ty::PAD_LEAVE_RESULT if p.len() >= 2 => Inbound::PadLeaveResult { pad: p[0], accepted: p[1] == 0 },
            ty::PASSCODE_REQUEST if b(0).is_some() => Inbound::PasscodeRequest(p[0]),
            ty::USER_JOIN_RESULT if p.len() >= 2 => Inbound::UserJoinResult { pad: p[0], status: p[1] },
            ty::IDENTITY if p.len() == 24 => Inbound::AccountUnknown,
            ty::KEEPALIVE => Inbound::Keepalive,
            _ => Inbound::Unknown { ty, len: p.len() },
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum FrameError {
    /// Declared payload length is implausible: the stream is out of sync or hostile.
    TooLarge(usize),
    /// The inbound counter would overflow; the session must be torn down.
    CounterExhausted,
}

/// Reassembles frames from a byte stream and decrypts them with an inbound counter that re-anchors on desync.
pub struct CtrlRx {
    cipher: RpCipher,
    counter: u32,
    buf: Vec<u8>,
}

/// How far the inbound counter may drift before we give up (the app searches +-32).
pub const RESYNC_WINDOW: u32 = 32;

impl CtrlRx {
    pub fn new(cipher: RpCipher, counter: u32) -> Self {
        Self { cipher, counter, buf: vec![] }
    }

    pub fn counter(&self) -> u32 {
        self.counter
    }

    pub fn feed(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
    }

    /// Next complete frame, decoded. `Ok(None)` = need more bytes.
    pub fn next_message(&mut self) -> Result<Option<Inbound>, FrameError> {
        if self.buf.len() < HEADER_LEN {
            return Ok(None);
        }
        let len = u32::from_be_bytes([self.buf[0], self.buf[1], self.buf[2], self.buf[3]]) as usize;
        if len > MAX_PAYLOAD {
            return Err(FrameError::TooLarge(len));
        }
        if self.buf.len() < HEADER_LEN + len {
            return Ok(None);
        }
        let ty = u16::from_be_bytes([self.buf[4], self.buf[5]]);
        let raw: Vec<u8> = self.buf[HEADER_LEN..HEADER_LEN + len].to_vec();
        self.buf.drain(..HEADER_LEN + len);
        if raw.is_empty() {
            return Ok(Some(Inbound::parse(ty, &raw)));
        }
        let full = |ctr: u32| {
            let mut p = raw.clone();
            self.cipher.decrypt(&mut p, ctr);
            Inbound::parse(ty, &p)
        };
        // plausibility only depends on the first two bytes, so probe with a 2-byte prefix (cheap)
        let probe = |ctr: u32| {
            let mut p = raw[..raw.len().min(2)].to_vec();
            self.cipher.decrypt(&mut p, ctr);
            Inbound::parse(ty, &p).plausible()
        };
        let next = self.counter.checked_add(1).ok_or(FrameError::CounterExhausted)?;
        if probe(self.counter) {
            let m = full(self.counter);
            self.counter = next;
            return Ok(Some(m));
        }
        // a pad index >= 4 means we are out of step: search nearby counters for a plausible decode
        let hi = self.counter.saturating_add(RESYNC_WINDOW);
        for d in 1..=RESYNC_WINDOW {
            for ctr in [self.counter.checked_add(d), self.counter.checked_sub(d)].into_iter().flatten() {
                if ctr > hi || ctr == u32::MAX {
                    continue;
                }
                if probe(ctr) {
                    let m = full(ctr);
                    self.counter = ctr + 1;
                    return Ok(Some(m));
                }
            }
        }
        let m = full(self.counter);
        self.counter = next;
        Ok(Some(m)) // nothing plausible nearby: surface it as decoded
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cipher() -> RpCipher {
        RpCipher::new([1; 16], [2; 16], [3; 16])
    }

    #[test]
    fn outbound_frames_have_the_apps_headers_and_encrypt_payloads_in_order() {
        let mut tx = CtrlTx::new(cipher(), 4);
        let pin = tx.pin("1234");
        assert_eq!(&pin[..8], &[0, 0, 0, 4, 0x80, 0x04, 0, 0]);
        let mic = tx.mic_request();
        assert_eq!(&mic[..8], &[0, 0, 0, 2, 0x00, 0x30, 0, 0]);
        let st = tx.mic_status(true);
        assert_eq!(&st[..8], &[0, 0, 0, 4, 0x00, 0x36, 0, 0]);
        let rest = tx.rest_mode();
        assert_eq!(rest, vec![0, 0, 0, 0, 0x00, 0x50, 0, 0]); // no payload, no encryption, no counter
        assert_eq!(tx.counter(), 7); // pin, mic request, mic status consumed 4, 5, 6
                                     // payloads decrypt with the matching counters
        let c = cipher();
        let mut p = pin[8..].to_vec();
        c.decrypt(&mut p, 4);
        assert_eq!(p, b"1234");
        let mut m = st[8..].to_vec();
        c.decrypt(&mut m, 6);
        assert_eq!(m, [0, 1, 1, 0x59]);
    }

    #[test]
    fn pad_join_codes() {
        let c = cipher();
        let join = |t: u8| {
            let f = CtrlTx::new(cipher(), 0).pad_join(2, t);
            let mut p = f[8..].to_vec();
            c.decrypt(&mut p, 0);
            p
        };
        assert_eq!((join(3), join(1), join(6), join(0)), (vec![2, 1], vec![2, 2], vec![2, 3], vec![2, 3]));
        let leave = CtrlTx::new(cipher(), 0).pad_leave(1);
        assert_eq!(&leave[..8], &[0, 0, 0, 1, 0x00, 0x09, 0, 0]);
    }

    fn inbound_frame(c: &RpCipher, ty: u16, payload: &[u8], ctr: u32) -> Vec<u8> {
        let mut p = payload.to_vec();
        if !p.is_empty() {
            c.encrypt(&mut p, ctr);
        }
        let mut f = (payload.len() as u32).to_be_bytes().to_vec();
        f.extend(ty.to_be_bytes());
        f.extend([0, 0]);
        f.extend(p);
        f
    }

    #[test]
    fn inbound_messages_decode_across_split_reads() {
        let c = cipher();
        let mut bytes = vec![];
        bytes.extend(inbound_frame(&c, ty::DISPLAY_STATUS, &[1], 0));
        bytes.extend(inbound_frame(&c, ty::PAD_JOIN_RESULT, &[1, 0], 1));
        bytes.extend(inbound_frame(&c, ty::SESSION_ID, b"sess-123\0", 2));
        bytes.extend(inbound_frame(&c, ty::KEEPALIVE, &[], 0));
        bytes.extend(inbound_frame(&c, ty::PASSCODE_REQUEST, &[2], 3));
        let mut rx = CtrlRx::new(cipher(), 0);
        let mut out = vec![];
        for chunk in bytes.chunks(3) {
            rx.feed(chunk);
            while let Some(m) = rx.next_message().unwrap() {
                out.push(m);
            }
        }
        assert_eq!(
            out,
            vec![
                Inbound::DisplayStatus { can_display: false }, // byte0 == 1 means it cannot be displayed
                Inbound::PadJoinResult { pad: 1, accepted: true },
                Inbound::SessionId("sess-123".into()),
                Inbound::Keepalive,
                Inbound::PasscodeRequest(2),
            ]
        );
        assert_eq!(rx.counter(), 4); // the empty keepalive consumed nothing
    }

    #[test]
    fn message_semantics() {
        assert_eq!(Inbound::parse(ty::DISPLAY_STATUS, &[0]), Inbound::DisplayStatus { can_display: true });
        assert_eq!(Inbound::parse(ty::PIN_RESULT, &[2]), Inbound::PinResult(2));
        assert_eq!(Inbound::parse(ty::PAD_JOIN_RESULT, &[0, 1]), Inbound::PadJoinResult { pad: 0, accepted: false });
        assert_eq!(Inbound::parse(ty::PAD_LEAVE_RESULT, &[3, 0]), Inbound::PadLeaveResult { pad: 3, accepted: true });
        assert_eq!(Inbound::parse(ty::USER_JOIN_RESULT, &[1, 3]), Inbound::UserJoinResult { pad: 1, status: 3 });
        assert_eq!(Inbound::parse(ty::STREAMABLE, &[1, 0xFF]), Inbound::Streamable(false));
        assert_eq!(Inbound::parse(ty::STREAMABLE, &[0, 0]), Inbound::Streamable(true));
        assert_eq!(Inbound::parse(ty::IDENTITY, &[0; 24]), Inbound::AccountUnknown);
        assert_eq!(Inbound::parse(ty::IDENTITY, &[0; 10]), Inbound::Unknown { ty: 97, len: 10 });
        assert_eq!(Inbound::parse(9999, &[1, 2]), Inbound::Unknown { ty: 9999, len: 2 });
        assert_eq!(Inbound::parse(ty::PAD_DROPPED, &[]), Inbound::Unknown { ty: 10, len: 0 }); // too short to be a pad message
        assert!(
            Inbound::PadDropped(3).plausible()
                && !Inbound::PadDropped(4).plausible()
                && Inbound::PinRequired.plausible()
        );
        assert_eq!(KEEPALIVE_REPLY, [0, 0, 0, 0, 1, 0xFE, 0, 0]);
    }

    #[test]
    fn the_inbound_counter_resynchronises_after_a_missed_frame() {
        let c = cipher();
        // the console sent one frame we never saw (counter 0), then a pad message at counter 1
        let mut rx = CtrlRx::new(cipher(), 0);
        rx.feed(&inbound_frame(&c, ty::PAD_DROPPED, &[2], 3)); // we expect counter 0 but it was encrypted at 3
        match rx.next_message().unwrap() {
            Some(Inbound::PadDropped(2)) => {}
            other => panic!("expected resync to recover PadDropped(2), got {other:?}"),
        }
        assert_eq!(rx.counter(), 4);
    }

    #[test]
    fn hostile_lengths_are_rejected_without_allocating() {
        let mut rx = CtrlRx::new(cipher(), 0);
        rx.feed(&[0xFF, 0xFF, 0xFF, 0xFF, 0, 1, 0, 0]);
        assert_eq!(rx.next_message(), Err(FrameError::TooLarge(u32::MAX as usize)));
        let mut rx = CtrlRx::new(cipher(), 0);
        rx.feed(&[0, 0, 0, 4, 0, 1, 0, 0, 1, 2]); // incomplete
        assert_eq!(rx.next_message(), Ok(None));
    }
}
