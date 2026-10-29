//! SCTP-like Takion control chunk (spec 02 section 5.1; `ControlMessage.java`). All fields big-endian.
//!
//! ```text
//! 0 u8 type|subtype<<4 (0 = control)   1 u32 tag   5 u32 gmac   9 u32 keyPos
//! 13 u8 chunkType   14 u8 chunkFlags   15 u16 chunkSize (= 12 + payload len)
//! 17 u32 sequence   21 u32 classValue  25 payload   (sequence/class present when chunkSize > 4)
//! ```
//! When `chunkFlags == 1` the first payload byte is the payload kind (0 = protobuf; 7/9/11 = pad blobs).

pub const GMAC_OFFSET: usize = 5;
pub const KEY_POS_OFFSET: usize = 9;

pub mod chunk_type {
    pub const DATA: u8 = 0;
    pub const INIT: u8 = 1;
    pub const INIT_ACK: u8 = 2;
    pub const DATA_ACK: u8 = 3;
    pub const COOKIE: u8 = 10;
    pub const COOKIE_ACK: u8 = 11;
}

/// `classValue = channel<<16 | low16` (spec 02 5.1); replayed verbatim.
pub mod class {
    pub const GENERAL: u32 = 65536;
    pub const CORRUPT_FRAME: u32 = 131072;
    pub const SENKUSHA_ECHO: u32 = 524288;
    pub const STREAM_INFO: u32 = 589824;
    pub const PROTOCOL_REQUEST: u32 = 1_376_256; // 0x150000
    pub const CONTROL: u32 = 102_400; // 0x19000: INIT, COOKIE and every DATA_ACK
}

/// Payload of every DATA_ACK.
pub const ACK_PAYLOAD: [u8; 4] = [0, 0, 0, 0];

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Chunk {
    pub tag: u32,
    pub gmac: u32,
    pub key_pos: u32,
    pub chunk_type: u8,
    pub flags: u8,
    pub sequence: u32,
    pub class: u32,
    /// First payload byte when `flags == 1` (payload kind), else None.
    pub payload_kind: Option<u8>,
    /// Payload (without the kind byte when `flags == 1`).
    pub payload: Vec<u8>,
}

impl Chunk {
    pub fn new(tag: u32, chunk_type: u8, flags: u8, sequence: u32, class: u32, payload: Vec<u8>) -> Self {
        Self { tag, chunk_type, flags, sequence, class, payload, ..Default::default() }
    }

    /// `chunkSize` field as the app computes it: 12 + the serialised payload length (kind byte included).
    pub fn chunk_size(&self) -> usize {
        12 + self.payload.len() + self.payload_kind.map_or(0, |_| 1)
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(25 + self.payload.len());
        v.push(0);
        v.extend(self.tag.to_be_bytes());
        v.extend(self.gmac.to_be_bytes());
        v.extend(self.key_pos.to_be_bytes());
        v.push(self.chunk_type);
        v.push(self.flags);
        v.extend((self.chunk_size() as u16).to_be_bytes());
        v.extend(self.sequence.to_be_bytes());
        v.extend(self.class.to_be_bytes());
        if let Some(k) = self.payload_kind {
            v.push(k);
        }
        v.extend(&self.payload);
        v
    }

    /// Build a DATA chunk carrying `payload` where its first byte is the payload kind (what the app does:
    /// `concatenateArrays(new byte[1], protobuf)`).
    pub fn data(tag: u32, sequence: u32, class: u32, kind_and_payload: Vec<u8>) -> Self {
        let (kind, rest) =
            kind_and_payload.split_first().map(|(k, r)| (Some(*k), r.to_vec())).unwrap_or((None, vec![]));
        Self {
            tag,
            chunk_type: chunk_type::DATA,
            flags: 1,
            sequence,
            class,
            payload_kind: kind,
            payload: rest,
            ..Default::default()
        }
    }

    pub fn ack(tag: u32, sequence: u32) -> Self {
        Self::new(tag, chunk_type::DATA_ACK, 0, sequence, class::CONTROL, ACK_PAYLOAD.to_vec())
    }

    pub fn decode(b: &[u8]) -> Option<Chunk> {
        if b.len() < 17 || b[0] & 0x0F != 0 {
            return None;
        } // control packets only
        let u32at = |o: usize| u32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
        let size = u16::from_be_bytes([b[15], b[16]]) as usize;
        let mut c = Chunk {
            tag: u32at(1),
            gmac: u32at(5),
            key_pos: u32at(9),
            chunk_type: b[13],
            flags: b[14],
            ..Default::default()
        };
        let mut off = 17;
        if size >= 8 {
            if b.len() < 25 {
                return None;
            }
            c.sequence = u32at(17);
            c.class = u32at(21);
            off = 25;
        }
        if off < b.len() {
            if c.flags == 1 {
                c.payload_kind = Some(b[off]);
                c.payload = b[off + 1..].to_vec();
            } else {
                c.payload = b[off..].to_vec();
            }
        }
        // the app's `payloadSize` field (= chunkSize), used by its reply predicates
        let _ = size;
        Some(c)
    }

    /// The `chunkSize` value as read off the wire for a decoded chunk.
    pub fn wire_size(&self) -> usize {
        self.chunk_size()
    }
}

pub fn is_control(b: &[u8]) -> bool {
    !b.is_empty() && b[0] & 0x0F == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_matches_the_apps_bytes() {
        let init = Chunk::new(0, chunk_type::INIT, 0, 0x4823, class::CONTROL, vec![0, 100, 0, 100, 0, 0, 0x48, 0x23]);
        assert_eq!(init.chunk_size(), 20);
        let b = init.encode();
        assert_eq!(b.len(), 33);
        assert_eq!(&b[..17], &[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 20]);
        assert_eq!(&b[17..25], &[0, 0, 0x48, 0x23, 0, 1, 0x90, 0x00]);
        assert_eq!(Chunk::decode(&b).unwrap(), init);
    }

    #[test]
    fn data_chunk_roundtrip_with_payload_kind() {
        let c = Chunk::data(7, 9, class::GENERAL, vec![0, 8, 3]);
        assert_eq!(c.flags, 1);
        assert_eq!(c.chunk_size(), 15);
        let d = Chunk::decode(&c.encode()).unwrap();
        assert_eq!((d.payload_kind, d.payload.as_slice()), (Some(0), &[8, 3][..]));
        assert_eq!(d, c);
    }

    #[test]
    fn ack_layout() {
        let a = Chunk::ack(0xAABBCCDD, 5);
        let b = a.encode();
        assert_eq!(b[13], chunk_type::DATA_ACK);
        assert_eq!(&b[15..17], &16u16.to_be_bytes());
        assert_eq!(&b[25..], &[0, 0, 0, 0]);
        assert_eq!(Chunk::decode(&b).unwrap().sequence, 5);
    }

    #[test]
    fn non_control_and_short_packets_are_rejected() {
        assert!(Chunk::decode(&[2; 30]).is_none()); // video
        assert!(Chunk::decode(&[0; 10]).is_none());
        assert!(is_control(&[0x10, 0]) && !is_control(&[3]));
    }
}
