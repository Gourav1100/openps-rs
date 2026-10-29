#![cfg(target_os = "macos")]
//! Tester A4: jitter buffer / AudioQueue player under odd configurations. Always volume 0 (silent); skipped when the
//! machine has no output device.
use openps_audio::header::AudioHeader;
use openps_audio::player::{Player, PlayerConfig};
use std::time::{Duration, Instant};

fn header(ch: u8, rate: u32, frame: u32) -> AudioHeader {
    let mut b = vec![ch, 16];
    b.extend(rate.to_be_bytes());
    b.extend(frame.to_be_bytes());
    b.extend(1u32.to_be_bytes());
    AudioHeader::parse(&b).unwrap()
}
fn player(h: AudioHeader, cfg: PlayerConfig) -> Option<Player> {
    match Player::new(h, cfg) {
        Ok(p) => {
            p.set_volume(0.0);
            assert_eq!(p.volume(), 0.0);
            Some(p)
        }
        Err(e) => {
            eprintln!("skipping, no usable audio output: {e}");
            None
        }
    }
}
fn stereo(frames: usize) -> Vec<i16> {
    (0..frames * 2).map(|i| (i as i16).wrapping_mul(3)).collect()
}
fn wait(limit: Duration, f: impl Fn() -> bool) -> bool {
    let end = Instant::now() + limit;
    while Instant::now() < end {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    false
}

#[test]
fn every_rate_and_channel_count_opens_and_plays_silently() {
    for (ch, rate) in
        [(1u8, 8000u32), (2, 8000), (1, 12000), (2, 16000), (1, 24000), (2, 24000), (1, 48000), (2, 48000)]
    {
        let h = header(ch, rate, rate / 100);
        let Some(p) = player(h, PlayerConfig::default()) else { return };
        let per_ms = (rate / 1000 * u32::from(ch)) as usize;
        p.push(&vec![1000i16; per_ms * 200]);
        assert!(
            wait(Duration::from_secs(2), || p.stats().samples_out > (per_ms * 40) as u64),
            "{ch}ch {rate}Hz: no output"
        );
        let s = p.stats();
        assert!(s.buffered <= (per_ms * 150) as u64 + 1, "{s:?}");
    }
}

#[test]
fn odd_configurations_do_not_panic() {
    let h = header(2, 48000, 480);
    let cfgs = [
        PlayerConfig { prefill_ms: 0, target_ms: 0, max_ms: 0, buffer_ms: 20, buffers: 3 },
        PlayerConfig { prefill_ms: 40, target_ms: 60, max_ms: 150, buffer_ms: 1, buffers: 0 },
        PlayerConfig { prefill_ms: 40, target_ms: 200, max_ms: 100, buffer_ms: 20, buffers: 3 }, // target > max
        PlayerConfig { prefill_ms: 10, target_ms: 20, max_ms: 30, buffer_ms: 500, buffers: 2 },
        PlayerConfig { prefill_ms: 1_000_000, target_ms: 2_000_000, max_ms: 3_000_000, buffer_ms: 20, buffers: 3 },
    ];
    for c in cfgs {
        match Player::new(h, c) {
            Ok(p) => {
                p.set_volume(0.0);
                p.push(&stereo(4800));
                std::thread::sleep(Duration::from_millis(60));
                let _ = p.stats();
                p.flush();
            }
            Err(e) => eprintln!("{c:?}: refused ({e})"),
        }
    }
    // zero-length buffers: refused or accepted, never UB-looking crashes
    let r = Player::new(h, PlayerConfig { buffer_ms: 0, ..Default::default() });
    eprintln!("buffer_ms = 0: {}", if r.is_ok() { "accepted" } else { "refused" });
}

#[test]
fn prefill_beyond_max_is_clamped_so_that_playback_starts() {
    let cfg = PlayerConfig { prefill_ms: 200, target_ms: 60, max_ms: 150, buffer_ms: 20, buffers: 3 };
    let Some(p) = player(header(2, 48000, 480), cfg) else { return };
    for _ in 0..100 {
        p.push(&stereo(480));
        std::thread::sleep(Duration::from_millis(5));
    }
    // stop feeding: a working player drains the ring (and then counts an underrun)
    assert!(wait(Duration::from_secs(3), || p.stats().buffered == 0), "ring never drained: {:?}", p.stats());
}

#[test]
fn overrun_trims_to_the_target_and_counts_once_per_trim() {
    let cfg = PlayerConfig { prefill_ms: 10_000, target_ms: 60, max_ms: 150, buffer_ms: 20, buffers: 3 }; // never starts: deterministic
    let Some(p) = player(header(2, 48000, 480), cfg) else { return };
    p.push(&stereo(48_000 * 5)); // 5 s in one call
    let s = p.stats();
    assert_eq!(s.overruns, 1);
    assert_eq!(s.buffered, 60 * 96, "trimmed to target_ms");
    // the *newest* audio is the one kept
    p.push(&stereo(1));
    assert_eq!(p.stats().buffered, 60 * 96 + 2);
    p.flush();
    assert_eq!(p.stats().buffered, 0);
}

#[test]
fn an_odd_number_of_samples_shifts_channels_for_all_later_audio_info() {
    // Info (T08-06): push() takes any &[i16]. The decoder always supplies whole frames, so this cannot happen on the
    // production path, but the PcmOut API does not enforce it: one odd-length push swaps L/R for the rest of the session
    // (until flush()). Documented by checking only that it is accepted and counted.
    let cfg = PlayerConfig { prefill_ms: 10_000, ..Default::default() };
    let Some(p) = player(header(2, 48000, 480), cfg) else { return };
    p.push(&[1, 2, 3]);
    assert_eq!(p.stats().buffered, 3);
}

#[test]
fn underrun_is_counted_once_per_dry_episode_not_per_device_buffer() {
    let Some(p) = player(header(2, 48000, 480), PlayerConfig::default()) else { return };
    p.push(&stereo(48 * 100)); // 100 ms
    assert!(wait(Duration::from_secs(2), || p.stats().underruns >= 1));
    std::thread::sleep(Duration::from_millis(400)); // 20 more empty device buffers
    assert_eq!(p.stats().underruns, 1, "re-entering prefill must stop the count");
    // refill: plays again, and a second dry spell is a second underrun
    p.push(&stereo(48 * 100));
    assert!(wait(Duration::from_secs(2), || p.stats().underruns >= 2));
}

#[test]
fn producer_floods_and_flush_races_never_corrupt_or_deadlock() {
    let Some(p) = player(header(2, 48000, 480), PlayerConfig::default()) else { return };
    let p = std::sync::Arc::new(p);
    let hs: Vec<_> = (0..4)
        .map(|t| {
            let p = p.clone();
            std::thread::spawn(move || {
                for i in 0..500 {
                    match (i + t) % 7 {
                        0 => p.flush(),
                        1 => p.set_volume(f32::NAN),
                        2 => p.set_volume(0.0),
                        3 => {
                            let _ = p.stats();
                        }
                        _ => p.push(&stereo(480)),
                    }
                }
            })
        })
        .collect();
    for h in hs {
        h.join().unwrap();
    }
    assert_eq!(p.volume(), 0.0);
    assert!(p.stats().buffered <= 150 * 96);
}

#[test]
fn create_destroy_cycles_do_not_leak_memory() {
    fn rss_mb() -> f64 {
        #[repr(C)]
        struct R {
            t: [i64; 4],
            maxrss: i64,
            rest: [i64; 13],
        }
        extern "C" {
            fn getrusage(w: i32, u: *mut R) -> i32;
        }
        let mut r = R { t: [0; 4], maxrss: 0, rest: [0; 13] };
        unsafe { getrusage(0, &mut r) };
        r.maxrss as f64 / 1048576.0
    }
    let h = header(2, 48000, 480);
    let Some(first) = player(h, PlayerConfig::default()) else { return };
    drop(first);
    let before = rss_mb();
    for _ in 0..60 {
        let Some(p) = player(h, PlayerConfig::default()) else { return };
        p.push(&stereo(4800));
    }
    assert!(rss_mb() - before < 60.0, "{before} -> {}", rss_mb());
}
