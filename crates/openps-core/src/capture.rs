//! Opt-in capture of the console's video (`OPENPS_VIDEO_CAPTURE=<directory>`), for studying loss recovery offline: every access unit
//! as it left the reassembler, with its frame id, adaptive-stream index and arrival time, plus the codec parameter sets from the
//! stream info. A hole in the frame ids is a loss the stream layer reported to the console. The capture stops by itself after
//! `MAX_SECONDS` or `MAX_BYTES`. It holds the picture content, so it is for the user's own debugging only and never leaves the machine.
//!
//! File: `MAGIC`, then records of `kind: u8` (0 = frame, 1 = parameter sets), `id: u16`, `adaptive: u8`, `at_us: u64`, `len: u32`
//! (all little-endian), `data`.

use openps_stream::video::Frame;
use std::fs::File;
use std::io::{self, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

pub const MAGIC: &[u8; 8] = b"OPSVID1\n";
pub const MAX_SECONDS: u64 = 90;
pub const MAX_BYTES: u64 = 200_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Frame,
    ParameterSets,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub kind: Kind,
    pub id: u16,
    pub adaptive: u8,
    pub at_us: u64,
    pub data: Vec<u8>,
}

pub struct VideoCapture {
    out: BufWriter<File>,
    path: PathBuf,
    started: Instant,
    bytes: u64,
    max_seconds: u64,
    max_bytes: u64,
    done: bool,
}

impl VideoCapture {
    /// A capture in the directory named by `OPENPS_VIDEO_CAPTURE`, if it is set and the file can be created.
    /// `OPENPS_VIDEO_CAPTURE_SECONDS` and `OPENPS_VIDEO_CAPTURE_MB` raise the default limits for a long recording.
    pub fn from_env() -> Option<Self> {
        let dir = PathBuf::from(std::env::var_os("OPENPS_VIDEO_CAPTURE").filter(|d| !d.is_empty())?);
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let path = dir.join(format!("video-{stamp}.opsvid"));
        let seconds = env_limit("OPENPS_VIDEO_CAPTURE_SECONDS", MAX_SECONDS, 1);
        let bytes = env_limit("OPENPS_VIDEO_CAPTURE_MB", MAX_BYTES, 1_000_000);
        match Self::create(&path, seconds, bytes) {
            Ok(c) => {
                log::info!("capturing video to {}", path.display());
                Some(c)
            }
            Err(e) => {
                log::warn!("cannot start the video capture in {}: {e}", dir.display());
                None
            }
        }
    }

    pub fn create(path: &Path, max_seconds: u64, max_bytes: u64) -> io::Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut out = BufWriter::new(File::create(path)?);
        out.write_all(MAGIC)?;
        Ok(Self { out, path: path.into(), started: Instant::now(), bytes: 0, max_seconds, max_bytes, done: false })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn is_done(&self) -> bool {
        self.done
    }

    pub fn parameter_sets(&mut self, header: &[u8]) {
        self.write(Kind::ParameterSets, 0, 0, header);
    }

    pub fn frame(&mut self, f: &Frame) {
        self.write(Kind::Frame, f.id, f.adaptive_index, &f.data);
    }

    fn write(&mut self, kind: Kind, id: u16, adaptive: u8, data: &[u8]) {
        if self.done {
            return;
        }
        let at_us = self.started.elapsed().as_micros() as u64;
        if self.started.elapsed().as_secs() >= self.max_seconds || self.bytes + data.len() as u64 > self.max_bytes {
            self.finish("limit reached");
            return;
        }
        let mut head = [0u8; 16];
        head[0] = u8::from(kind == Kind::ParameterSets);
        head[1..3].copy_from_slice(&id.to_le_bytes());
        head[3] = adaptive;
        head[4..12].copy_from_slice(&at_us.to_le_bytes());
        head[12..16].copy_from_slice(&(data.len() as u32).to_le_bytes());
        if let Err(e) = self.out.write_all(&head).and_then(|()| self.out.write_all(data)) {
            self.finish(&format!("write failed: {e}"));
            return;
        }
        self.bytes += data.len() as u64;
    }

    fn finish(&mut self, why: &str) {
        self.done = true;
        let _ = self.out.flush();
        log::info!("video capture stopped ({why}): {} bytes in {}", self.bytes, self.path.display());
    }
}

impl Drop for VideoCapture {
    fn drop(&mut self) {
        if !self.done {
            self.finish("session ended");
        }
    }
}

/// A positive whole number in the environment variable `name`, times `scale`; the default when unset or invalid.
fn env_limit(name: &str, default: u64, scale: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|&v| v > 0)
        .map_or(default, |v| v.saturating_mul(scale))
}

pub fn read(path: &Path) -> io::Result<Vec<Record>> {
    let mut f = io::BufReader::new(File::open(path)?);
    let mut magic = [0u8; 8];
    f.read_exact(&mut magic)?;
    if &magic != MAGIC {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "not a video capture"));
    }
    let mut out = vec![];
    loop {
        let mut head = [0u8; 16];
        match f.read_exact(&mut head) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(out), // a capture cut short is still usable
            Err(e) => return Err(e),
        }
        let len = u32::from_le_bytes([head[12], head[13], head[14], head[15]]) as usize;
        let mut data = vec![0u8; len];
        if f.read_exact(&mut data).is_err() {
            return Ok(out);
        }
        out.push(Record {
            kind: if head[0] == 1 { Kind::ParameterSets } else { Kind::Frame },
            id: u16::from_le_bytes([head[1], head[2]]),
            adaptive: head[3],
            at_us: u64::from_le_bytes(head[4..12].try_into().unwrap()),
            data,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn au(id: u16, adaptive: u8, n: usize) -> Frame {
        Frame { id, adaptive_index: adaptive, data: vec![id as u8; n], starts_with_start_code: true }
    }

    fn temp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("openps-capture-test-{}-{name}.opsvid", std::process::id()))
    }

    #[test]
    fn a_limit_comes_from_the_environment_only_when_it_is_a_positive_number() {
        std::env::set_var("OPENPS_TEST_LIMIT_OK", " 30 ");
        std::env::set_var("OPENPS_TEST_LIMIT_ZERO", "0");
        std::env::set_var("OPENPS_TEST_LIMIT_BAD", "abc");
        assert_eq!(env_limit("OPENPS_TEST_LIMIT_OK", 90, 1_000), 30_000);
        assert_eq!(env_limit("OPENPS_TEST_LIMIT_ZERO", 90, 1), 90);
        assert_eq!(env_limit("OPENPS_TEST_LIMIT_BAD", 90, 1), 90);
        assert_eq!(env_limit("OPENPS_TEST_LIMIT_UNSET", 90, 1), 90);
    }

    #[test]
    fn frames_and_parameter_sets_round_trip_with_ids_and_a_rising_clock() {
        let p = temp("roundtrip");
        let mut c = VideoCapture::create(&p, 60, 1 << 20).unwrap();
        c.parameter_sets(&[0, 0, 0, 1, 0x67]);
        c.frame(&au(10, 0, 5));
        c.frame(&au(12, 2, 7)); // 11 was lost
        drop(c);
        let r = read(&p).unwrap();
        std::fs::remove_file(&p).ok();
        assert_eq!(r.len(), 3);
        assert_eq!((r[0].kind, r[0].data.clone()), (Kind::ParameterSets, vec![0, 0, 0, 1, 0x67]));
        assert_eq!((r[1].kind, r[1].id, r[1].adaptive, r[1].data.len()), (Kind::Frame, 10, 0, 5));
        assert_eq!((r[2].id, r[2].adaptive, r[2].data.len()), (12, 2, 7), "the hole in the ids is kept");
        assert!(r[2].at_us >= r[1].at_us);
    }

    #[test]
    fn the_capture_stops_at_its_byte_limit_and_keeps_what_it_has() {
        let p = temp("limit");
        let mut c = VideoCapture::create(&p, 60, 100).unwrap();
        c.frame(&au(1, 0, 60));
        assert!(!c.is_done());
        c.frame(&au(2, 0, 60)); // would pass 100 bytes
        assert!(c.is_done());
        c.frame(&au(3, 0, 1));
        drop(c);
        let r = read(&p).unwrap();
        std::fs::remove_file(&p).ok();
        assert_eq!(r.iter().map(|x| x.id).collect::<Vec<_>>(), vec![1]);
    }

    #[test]
    fn a_zero_second_limit_records_nothing() {
        let p = temp("time");
        let mut c = VideoCapture::create(&p, 0, 1 << 20).unwrap();
        c.frame(&au(1, 0, 4));
        assert!(c.is_done());
        drop(c);
        let r = read(&p).unwrap();
        std::fs::remove_file(&p).ok();
        assert!(r.is_empty());
    }

    #[test]
    fn a_truncated_file_yields_the_complete_records_and_a_foreign_file_is_refused() {
        let p = temp("cut");
        let mut c = VideoCapture::create(&p, 60, 1 << 20).unwrap();
        c.frame(&au(1, 0, 50));
        c.frame(&au(2, 0, 50));
        drop(c);
        let full = std::fs::read(&p).unwrap();
        std::fs::write(&p, &full[..full.len() - 20]).unwrap();
        assert_eq!(read(&p).unwrap().len(), 1);
        std::fs::write(&p, b"something else entirely").unwrap();
        assert!(read(&p).is_err());
        std::fs::remove_file(&p).ok();
    }
}
