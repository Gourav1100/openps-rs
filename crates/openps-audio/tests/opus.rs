#![cfg(target_os = "macos")]
use openps_audio::header::AudioHeader;
use openps_audio::opus::{OpusDecoder, OpusError};
use openps_testkit::ogg;

fn load(name: &str) -> (Vec<u8>, Vec<Vec<u8>>) {
    let d = std::fs::read(format!("{}/tests/data/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap();
    let p = ogg::opus_packets(&d).expect("ogg opus");
    (d, p)
}

fn header(ch: u8) -> AudioHeader {
    let mut b = vec![ch, 16];
    b.extend(48000u32.to_be_bytes());
    b.extend(480u32.to_be_bytes());
    b.extend(1u32.to_be_bytes());
    AudioHeader::parse(&b).unwrap()
}

/// The steady part of a decode: drops the first 20 ms (priming) and the last 30 ms (the encoder's end padding, which
/// players trim using the container's end position; raw packets carry no such marker).
fn steady(pcm: &[i16], channels: usize) -> &[i16] {
    &pcm[960 * channels..pcm.len() - 1440 * channels]
}

/// Least-squares fit of `a*sin(wt) + b*cos(wt)` to channel `ch`; returns (amplitude, residual RMS) over `skip..`.
fn fit(pcm: &[i16], channels: usize, ch: usize, hz: f64, skip: usize) -> (f64, f64) {
    let xs: Vec<f64> = pcm.iter().skip(ch).step_by(channels).map(|&s| f64::from(s) / 32768.0).collect();
    let w = 2.0 * std::f64::consts::PI * hz / 48000.0;
    let (mut ss, mut cc, mut sc, mut sx, mut cx) = (0.0, 0.0, 0.0, 0.0, 0.0);
    for (n, &x) in xs.iter().enumerate().skip(skip) {
        let (s, c) = ((w * n as f64).sin(), (w * n as f64).cos());
        ss += s * s;
        cc += c * c;
        sc += s * c;
        sx += s * x;
        cx += c * x;
    }
    let det = ss * cc - sc * sc;
    let (a, b) = ((sx * cc - cx * sc) / det, (cx * ss - sx * sc) / det);
    let mut err = 0.0;
    let mut n_used = 0.0;
    for (n, &x) in xs.iter().enumerate().skip(skip) {
        let m = a * (w * n as f64).sin() + b * (w * n as f64).cos();
        err += (x - m).powi(2);
        n_used += 1.0;
    }
    ((a * a + b * b).sqrt(), (err / n_used).sqrt())
}

fn decode_all(h: AudioHeader, packets: &[Vec<u8>]) -> Vec<i16> {
    let mut d = OpusDecoder::new(h).expect("decoder");
    let mut pcm = vec![];
    for (i, p) in packets.iter().enumerate() {
        d.decode(p, &mut pcm).unwrap_or_else(|e| panic!("packet {i} ({} bytes): {e}", p.len()));
    }
    pcm
}

#[test]
fn stereo_tones_decode_with_correct_frequency_amplitude_and_channel_order() {
    let (data, packets) = load("stereo_440_880.opus");
    assert_eq!(ogg::head(&data).map(|h| h.0), Some(2));
    assert!(packets.len() >= 190, "2 s of 10 ms frames, got {}", packets.len());
    assert!(packets.iter().all(|p| p.len() <= 80), "frames must stay under the haptics threshold (80 B)");
    let pcm = decode_all(header(2), &packets);
    assert_eq!(pcm.len(), packets.len() * 480 * 2);
    // Measured against libopus (ffmpeg) on this very file: aligned by the 120-sample pre-skip the two decodes differ by
    // 2e-5 RMS (under one LSB), and both show a ~0.007 residual against an ideal sine (codec noise at 48 kbps).
    let s = steady(&pcm, 2);
    let (la, lres) = fit(s, 2, 0, 440.0, 0);
    let (ra, rres) = fit(s, 2, 1, 880.0, 0);
    assert!((la - 0.5).abs() < 0.02, "left amplitude {la}");
    assert!((ra - 0.5).abs() < 0.02, "right amplitude {ra}");
    assert!(lres < 0.012 && rres < 0.012, "residual left {lres} right {rres}: not a clean sine");
    // swapped channels must NOT fit: proves left is left
    let (xa, _) = fit(s, 2, 0, 880.0, 0);
    assert!(xa < 0.05, "left channel contains the right tone ({xa})");
}

#[test]
fn mono_tone_decodes() {
    let (data, packets) = load("mono_660.opus");
    assert_eq!(ogg::head(&data).map(|h| h.0), Some(1));
    let pcm = decode_all(header(1), &packets);
    assert_eq!(pcm.len(), packets.len() * 480);
    // libopus itself leaves a 0.04 residual at this 24 kbps mono setting
    let (a, res) = fit(steady(&pcm, 1), 1, 0, 660.0, 0);
    assert!((a - 0.5).abs() < 0.03 && res < 0.05, "amp {a} residual {res}");
}

#[test]
fn bad_packets_error_cleanly_and_the_decoder_keeps_working() {
    let (_, packets) = load("stereo_440_880.opus");
    let mut d = OpusDecoder::new(header(2)).unwrap();
    let mut pcm = vec![];
    assert_eq!(d.decode(&[], &mut pcm), Err(OpusError::BadPacket));
    assert_eq!(d.decode(&vec![0u8; 5000], &mut pcm), Err(OpusError::BadPacket));
    assert!(pcm.is_empty(), "failed decodes append nothing");
    // random garbage: any result is acceptable, a crash is not
    let mut x = 0x9E37_79B9_7F4A_7C15u64;
    for round in 0..3000 {
        let n = 1 + (round % 120);
        let junk: Vec<u8> = (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x as u8
            })
            .collect();
        let before = pcm.len();
        match d.decode(&junk, &mut pcm) {
            Ok(()) => assert_eq!(pcm.len(), before + 960),
            Err(_) => assert_eq!(pcm.len(), before),
        }
        pcm.clear();
    }
    // and real audio still decodes afterwards
    d.reset();
    for p in &packets[..20] {
        d.decode(p, &mut pcm).unwrap();
    }
    assert_eq!(pcm.len(), 20 * 960);
}

#[test]
fn a_lost_frame_does_not_derail_later_frames() {
    let (_, packets) = load("stereo_440_880.opus");
    let mut kept: Vec<Vec<u8>> = packets.clone();
    let lost_at: Vec<usize> = (10..120).step_by(17).collect();
    for &i in &lost_at {
        kept[i].clear();
    }
    let mut d = OpusDecoder::new(header(2)).unwrap();
    let mut pcm = vec![];
    let mut errors = 0;
    for p in &kept {
        if d.decode(p, &mut pcm).is_err() {
            errors += 1;
        }
    }
    assert_eq!(errors, lost_at.len(), "exactly the missing packets fail");
    // after the last loss the decoder is back to a clean tone (the last 40 frames contain no gap)
    let tail = &pcm[pcm.len() - 960 * 70..pcm.len() - 960 * 30]; // 40 frames, clear of the encoder's end padding
    let (a, res) = fit(tail, 2, 0, 440.0, 0);
    assert!((a - 0.5).abs() < 0.02 && res < 0.012, "amp {a} res {res}");
}

#[test]
fn the_first_frame_is_padded_not_short_and_reset_restores_that() {
    let (_, packets) = load("mono_660.opus");
    let mut d = OpusDecoder::new(header(1)).unwrap();
    let mut pcm = vec![];
    d.decode(&packets[0], &mut pcm).unwrap();
    assert_eq!(pcm.len(), 480, "whole frame");
    assert!(pcm[..100].iter().all(|&s| s == 0), "padding is at the front");
    d.decode(&packets[1], &mut pcm).unwrap();
    assert_eq!(pcm.len(), 960);
    d.reset();
    d.decode(&packets[0], &mut pcm).unwrap(); // short again after reset, padded again
    assert_eq!(pcm.len(), 1440);
}
