//! The real AudioQueue, played at volume 0: the whole callback/ring path runs, nothing is audible.
#![cfg(target_os = "macos")]
use openps_audio::header::AudioHeader;
use openps_audio::player::{Player, PlayerConfig};
use std::time::{Duration, Instant};

fn header() -> AudioHeader {
    let mut b = vec![2u8, 16];
    b.extend(48000u32.to_be_bytes());
    b.extend(480u32.to_be_bytes());
    b.extend(1u32.to_be_bytes());
    AudioHeader::parse(&b).unwrap()
}

fn tone(frames: usize) -> Vec<i16> {
    (0..frames)
        .flat_map(|n| {
            let v = ((n as f64 * 0.06).sin() * 8000.0) as i16;
            [v, v]
        })
        .collect()
}

fn player(cfg: PlayerConfig) -> Option<Player> {
    match Player::new(header(), cfg) {
        Ok(p) => {
            p.set_volume(0.0);
            Some(p)
        }
        Err(e) => {
            eprintln!("no audio output device here, skipping: {e}");
            None
        }
    }
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
fn steady_feed_plays_at_the_device_rate_without_underruns() {
    let Some(p) = player(PlayerConfig { prefill_ms: 150, target_ms: 200, max_ms: 600, ..Default::default() }) else {
        return;
    };
    // A producer paced by the wall clock, like the network is. (A fixed `sleep(10 ms)` loop really takes ~12 ms per
    // iteration, which feeds the device ~17% too slowly and starves it after about a second.)
    let chunk = tone(480); // 10 ms
    let t0 = Instant::now();
    let mut sent_frames = 0usize;
    while t0.elapsed() < Duration::from_millis(1200) {
        let due = (t0.elapsed().as_secs_f64() * 48000.0) as usize + 480; // stay one chunk ahead
        while sent_frames < due {
            p.push(&chunk);
            sent_frames += 480;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    let s = p.stats();
    let secs = t0.elapsed().as_secs_f64();
    let rate = s.samples_out as f64 / 2.0 / secs;
    assert!((40_000.0..56_000.0).contains(&rate), "device consumed {rate:.0} frames/s");
    assert_eq!(s.underruns, 0, "{s:?}");
    assert_eq!(s.overruns, 0, "{s:?}");
}

#[test]
fn running_dry_is_counted_and_playback_resumes_after_prefill() {
    let Some(p) = player(PlayerConfig { prefill_ms: 40, ..Default::default() }) else { return };
    p.push(&tone(48 * 100)); // 100 ms, then nothing
    assert!(wait(Duration::from_secs(2), || p.stats().underruns >= 1), "{:?}", p.stats());
    assert!(wait(Duration::from_secs(1), || p.stats().buffered == 0));
    // a trickle below the prefill threshold is held back, not played in fragments
    p.push(&tone(48 * 10)); // 10 ms
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(p.stats().buffered, 48 * 10 * 2, "below prefill: waits");
    // reaching the prefill level resumes consumption
    p.push(&tone(48 * 60));
    assert!(wait(Duration::from_secs(2), || p.stats().buffered == 0), "{:?}", p.stats());
}

#[test]
fn a_flood_is_trimmed_to_bound_latency() {
    let Some(p) = player(PlayerConfig { prefill_ms: 40, target_ms: 60, max_ms: 150, ..Default::default() }) else {
        return;
    };
    p.push(&tone(48000 * 2)); // two seconds at once
    let s = p.stats();
    assert!(s.overruns >= 1, "{s:?}");
    assert!(s.buffered <= 150 * 48 * 2, "buffered {} samples, cap is {}", s.buffered, 150 * 48 * 2);
    assert!(s.buffered >= 60 * 48 * 2 - 48 * 2 * 25, "trimmed to the target, not emptied: {s:?}");
    // minus what the device already took
}

#[test]
fn flush_clears_and_volume_is_sanitised() {
    let Some(p) = player(PlayerConfig::default()) else { return };
    p.push(&tone(48 * 30));
    p.flush();
    assert_eq!(p.stats().buffered, 0);
    p.set_volume(7.0);
    assert_eq!(p.volume(), 1.0);
    p.set_volume(-3.0);
    assert_eq!(p.volume(), 0.0);
    p.set_volume(f32::NAN);
    assert_eq!(p.volume(), 0.0);
    p.set_volume(0.5);
    assert_eq!(p.volume(), 0.5);
}

#[test]
fn creating_and_dropping_players_repeatedly_while_audio_flows_is_safe() {
    for i in 0..25 {
        let Some(p) = player(PlayerConfig { prefill_ms: 10, ..Default::default() }) else { return };
        for _ in 0..(i % 5) + 1 {
            p.push(&tone(480));
        }
        if i % 3 == 0 {
            std::thread::sleep(Duration::from_millis(15)); // let the callback run against live data
        }
        drop(p); // must dispose the queue before freeing what its callback reads
    }
}

#[test]
fn concurrent_producers_and_the_device_thread_do_not_corrupt_the_ring() {
    let Some(p) = player(PlayerConfig { prefill_ms: 20, max_ms: 100, target_ms: 50, ..Default::default() }) else {
        return;
    };
    let p = std::sync::Arc::new(p);
    let hs: Vec<_> = (0..4)
        .map(|_| {
            let p = p.clone();
            std::thread::spawn(move || {
                for _ in 0..200 {
                    p.push(&tone(240));
                    std::thread::sleep(Duration::from_micros(500));
                }
            })
        })
        .collect();
    for h in hs {
        h.join().unwrap();
    }
    let s = p.stats();
    assert!(s.buffered <= 100 * 48 * 2, "{s:?}");
    assert!(s.samples_out > 0);
}
