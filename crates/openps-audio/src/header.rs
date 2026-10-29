//! The 14-byte audio header from the console's stream info (spec 02 section 8.1):
//! `u8 channels | u8 bitsPerSample | u32 sampleRate | u32 samplesPerFrame | u32 reserved` (big-endian).

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioHeader {
    pub channels: u8,
    pub bits: u8,
    pub sample_rate: u32,
    pub samples_per_frame: u32,
}

#[derive(Debug, PartialEq, Eq)]
pub enum HeaderError {
    Length(usize),
    Channels(u8),
    Bits(u8),
    Rate(u32),
    Frame(u32),
}
impl std::fmt::Display for HeaderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HeaderError::Length(n) => write!(f, "audio header is {n} bytes, expected 14"),
            HeaderError::Channels(c) => write!(f, "unsupported channel count {c} (mono and stereo only)"),
            HeaderError::Bits(b) => write!(f, "unsupported sample size {b} bits"),
            HeaderError::Rate(r) => write!(f, "unsupported sample rate {r}"),
            HeaderError::Frame(s) => write!(f, "unsupported frame size {s}"),
        }
    }
}
impl std::error::Error for HeaderError {}

impl AudioHeader {
    /// Strict: the original player only handles mono/stereo 16-bit, and Opus only exists at five sample rates with
    /// frames of 2.5..120 ms. Anything else from the network is rejected rather than handed to the decoder.
    pub fn parse(b: &[u8]) -> Result<Self, HeaderError> {
        if b.len() != 14 {
            return Err(HeaderError::Length(b.len()));
        }
        let (channels, bits) = (b[0], b[1]);
        let rate = u32::from_be_bytes(b[2..6].try_into().unwrap());
        let frame = u32::from_be_bytes(b[6..10].try_into().unwrap());
        if !(1..=2).contains(&channels) {
            return Err(HeaderError::Channels(channels));
        }
        if bits != 16 {
            return Err(HeaderError::Bits(bits));
        }
        if !matches!(rate, 8000 | 12000 | 16000 | 24000 | 48000) {
            return Err(HeaderError::Rate(rate));
        }
        // 2.5 ms .. 120 ms at this rate
        if frame < rate / 400 || frame > rate / 25 * 3 {
            return Err(HeaderError::Frame(frame));
        }
        Ok(Self { channels, bits, sample_rate: rate, samples_per_frame: frame })
    }

    pub fn frame_samples(&self) -> usize {
        self.samples_per_frame as usize * self.channels as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hdr(ch: u8, bits: u8, rate: u32, frame: u32) -> Vec<u8> {
        let mut v = vec![ch, bits];
        v.extend(rate.to_be_bytes());
        v.extend(frame.to_be_bytes());
        v.extend(1u32.to_be_bytes());
        v
    }

    #[test]
    fn accepts_the_documented_header_and_rejects_everything_unsupported() {
        let h = AudioHeader::parse(&hdr(2, 16, 48000, 480)).unwrap();
        assert_eq!((h.channels, h.sample_rate, h.samples_per_frame, h.frame_samples()), (2, 48000, 480, 960));
        assert!(AudioHeader::parse(&hdr(1, 16, 48000, 480)).is_ok());
        assert_eq!(AudioHeader::parse(&[0; 13]), Err(HeaderError::Length(13)));
        assert_eq!(AudioHeader::parse(&[0; 15]), Err(HeaderError::Length(15)));
        assert_eq!(AudioHeader::parse(&hdr(0, 16, 48000, 480)), Err(HeaderError::Channels(0)));
        assert_eq!(AudioHeader::parse(&hdr(6, 16, 48000, 480)), Err(HeaderError::Channels(6)));
        assert_eq!(AudioHeader::parse(&hdr(2, 8, 48000, 480)), Err(HeaderError::Bits(8)));
        assert_eq!(AudioHeader::parse(&hdr(2, 16, 44100, 480)), Err(HeaderError::Rate(44100)));
        assert_eq!(AudioHeader::parse(&hdr(2, 16, 48000, 0)), Err(HeaderError::Frame(0)));
        assert_eq!(AudioHeader::parse(&hdr(2, 16, 48000, 119)), Err(HeaderError::Frame(119)));
        assert!(AudioHeader::parse(&hdr(2, 16, 48000, 120)).is_ok()); // 2.5 ms
        assert!(AudioHeader::parse(&hdr(2, 16, 48000, 5760)).is_ok()); // 120 ms
        assert_eq!(AudioHeader::parse(&hdr(2, 16, 48000, 5761)), Err(HeaderError::Frame(5761)));
        assert_eq!(AudioHeader::parse(&hdr(2, 16, 48000, u32::MAX)), Err(HeaderError::Frame(u32::MAX)));
    }

    #[test]
    fn random_bytes_never_panic() {
        let mut x = 0xDEAD_BEEF_1234_5678u64;
        for _ in 0..20_000 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let b: Vec<u8> = (0..14).map(|i| (x >> (i * 4 % 56)) as u8).collect();
            let _ = AudioHeader::parse(&b);
        }
    }
}
