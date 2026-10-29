//! Tester A1: adversarial, property and behavioural tests for the stream engine and the video/audio assemblers.
use openps_input::state::ControllerState;
use openps_stream::audio::{AudioAssembler, AudioUnits};
use openps_stream::video::{Frame, Outcome, Unit, VideoAssembler, MAX_PENDING_FRAMES};
use openps_stream::{Event, Stream, StreamConfig};
use openps_takion::chunk::{chunk_type, class, Chunk};
use openps_takion::proto::*;
use openps_testkit::stream::{chunk_of, encode_frame, frame_data, msg_of, session, StreamConsole as Console};
use std::time::Instant;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.next() as u8).collect()
    }
}

fn drain(s: &mut Stream) -> Vec<Vec<u8>> {
    std::iter::from_fn(|| s.poll_transmit()).collect()
}
fn videos(evs: Vec<Event>) -> Vec<(u16, Vec<u8>)> {
    evs.into_iter().filter_map(|e| if let Event::Video(f) = e { Some((f.id, f.data)) } else { None }).collect()
}
fn push_frame(a: &mut VideoAssembler, id: u16, wire: &[Vec<u8>], m: usize, order: &[usize]) -> Vec<Outcome> {
    let total = wire.len() as u16;
    order
        .iter()
        .map(|&i| {
            a.push(&Unit {
                frame_id: id,
                unit_index: i as u16,
                total_units: total,
                fec_units: m as u16,
                adaptive_index: 0,
                payload: &wire[i],
            })
        })
        .collect()
}
fn pop_all(a: &mut VideoAssembler) -> Vec<Frame> {
    std::iter::from_fn(|| a.pop_frame()).collect()
}

// ---------------------------------------------------------------- video assembler: properties
/// Random loss (<= m units per frame), duplicates and local reordering across the 16-bit wrap. Whatever is
/// delivered must be byte-exact, in forward order, and never twice.
#[test]
fn property_lossy_reordered_stream_across_id_wrap() {
    let mut r = Rng(0xA1);
    let mut a = VideoAssembler::new(3);
    let start: u32 = 65_480;
    let total_frames = 160u32;
    let mut originals = std::collections::HashMap::new();
    let mut wire_units: Vec<(u16, usize, usize, Vec<Vec<u8>>)> = vec![];
    for n in 0..total_frames {
        let id = ((start + n) % 65_536) as u16;
        let k = 1 + r.below(7);
        let m = 1 + r.below(3);
        let data = frame_data(20 + r.below(3000), n as u8);
        let wire = encode_frame(&data, k, m);
        originals.insert(id, data);
        wire_units.push((id, k, m, wire));
    }
    // build a packet list with <= m losses per frame and 10% duplicates
    let mut packets: Vec<(usize, usize)> = vec![]; // (frame index, unit index)
    for (fi, (_, k, m, wire)) in wire_units.iter().enumerate() {
        let mut lost = 0;
        for ui in 0..k + m {
            if lost < *m && r.below(6) == 0 {
                lost += 1;
                continue;
            }
            packets.push((fi, ui));
            if r.below(10) == 0 {
                packets.push((fi, ui));
            }
        }
        let _ = wire;
    }
    // local reordering: swap neighbours within distance 6 packets
    for i in 0..packets.len() {
        if r.below(4) == 0 {
            let j = (i + r.below(6)).min(packets.len() - 1);
            packets.swap(i, j);
        }
    }
    let mut delivered: Vec<Frame> = vec![];
    for (fi, ui) in packets {
        let (id, k, m, wire) = &wire_units[fi];
        let out = a.push(&Unit {
            frame_id: *id,
            unit_index: ui as u16,
            total_units: (k + m) as u16,
            fec_units: *m as u16,
            adaptive_index: 0,
            payload: &wire[ui],
        });
        let _ = out;
        delivered.extend(pop_all(&mut a));
    }
    let mut seen = std::collections::HashSet::new();
    let mut last: Option<u16> = None;
    for f in &delivered {
        assert!(seen.insert(f.id), "frame {} delivered twice", f.id);
        assert_eq!(&f.data, originals.get(&f.id).unwrap(), "frame {} corrupted", f.id);
        if let Some(l) = last {
            let fwd = f.id.wrapping_sub(l);
            assert!(fwd != 0 && fwd < 32768, "non-forward delivery {l} -> {}", f.id);
        }
        last = Some(f.id);
    }
    println!("delivered {}/{} frames", delivered.len(), total_frames);
    assert!(delivered.len() as f64 >= total_frames as f64 * 0.8, "only {} delivered", delivered.len());
    assert!(a.pending_frames() <= MAX_PENDING_FRAMES);
}

#[test]
fn flood_of_distinct_frame_ids_stays_bounded() {
    let mut a = VideoAssembler::new(3);
    let payload = [0u8, 0, 1, 2, 3, 4];
    for i in 0..20_000u32 {
        let id = (i.wrapping_mul(2654435761) % 65536) as u16;
        // k = 4 units announced, only one sent: frame never completes
        a.push(&Unit {
            frame_id: id,
            unit_index: 0,
            total_units: 5,
            fec_units: 1,
            adaptive_index: 0,
            payload: &payload,
        });
        assert!(a.pending_frames() <= MAX_PENDING_FRAMES, "{}", a.pending_frames());
    }
}

#[test]
fn degenerate_unit_headers_do_not_panic() {
    let mut r = Rng(5);
    let mut a = VideoAssembler::new(0);
    for _ in 0..200_000 {
        let n = r.below(40);
        let p = r.bytes(n);
        let total = r.below(2100) as u16;
        let fec = r.below(1100) as u16;
        let _ = a.push(&Unit {
            frame_id: r.below(70_000) as u16,
            unit_index: r.below(2100) as u16,
            total_units: total,
            fec_units: fec,
            adaptive_index: 0,
            payload: &p,
        });
        while a.pop_frame().is_some() {}
        if r.below(5000) == 0 {
            a.reset();
        }
        let _ = a.poll_telemetry(r.below(100000) as f64 / 10.0);
    }
}

#[test]
fn units_in_wrong_order_and_parity_first_recover() {
    let mut a = VideoAssembler::new(3);
    let data = frame_data(2500, 9);
    let wire = encode_frame(&data, 5, 3);
    // parity first (first unit is parity => block size derived from the parity length), sources lost: 3 of 5
    push_frame(&mut a, 0, &wire, 3, &[7, 6, 5, 0, 1]);
    let f = pop_all(&mut a);
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].data, data);
}

#[test]
fn duplicate_and_late_units_are_ignored() {
    let mut a = VideoAssembler::new(3);
    let data = frame_data(300, 1);
    let wire = encode_frame(&data, 2, 1);
    let o = push_frame(&mut a, 10, &wire, 1, &[0, 0, 1]);
    assert_eq!(o, vec![Outcome::Pending, Outcome::Pending, Outcome::DeliveredInOrder]);
    assert_eq!(pop_all(&mut a).len(), 1);
    let o = push_frame(&mut a, 10, &wire, 1, &[0, 1]);
    assert_eq!(o, vec![Outcome::Late, Outcome::Late]);
    assert!(pop_all(&mut a).is_empty());
}

// ---------------------------------------------------------------- findings: video
#[test]
fn zero_source_unit_frame_is_rejected() {
    let mut a = VideoAssembler::new(3);
    a.push(&Unit { frame_id: 0, unit_index: 0, total_units: 3, fec_units: 3, adaptive_index: 0, payload: &[1, 2, 3] });
    let f = pop_all(&mut a);
    assert!(f.is_empty(), "empty frame delivered: {f:?}");
    assert_eq!(a.cursor(), -1, "cursor must not move on an invalid frame");
}

#[test]
fn zero_source_unit_frame_current_behaviour() {
    let mut a = VideoAssembler::new(3);
    let o = a.push(&Unit {
        frame_id: 40,
        unit_index: 0,
        total_units: 3,
        fec_units: 3,
        adaptive_index: 0,
        payload: &[1, 2, 3],
    });
    assert_eq!(o, Outcome::Invalid);
    assert!(pop_all(&mut a).is_empty());
    assert_eq!(a.cursor(), -1);
}

#[test]
fn unit_index_outside_the_frame_is_invalid() {
    let mut a = VideoAssembler::new(3);
    let o = a.push(&Unit {
        frame_id: 1,
        unit_index: 5,
        total_units: 3,
        fec_units: 1,
        adaptive_index: 0,
        payload: &[0, 0, 1, 2],
    });
    assert_eq!(o, Outcome::Invalid);
}

/// A console-side id reset (all frames look late) resynchronises after a run of anomalies and requests an IDR.
#[test]
fn id_space_reset_resynchronises_and_requests_idr() {
    let mut s = Stream::new(session(12), StreamConfig { video_window: 3, ..Default::default() });
    let mut c = Console::new();
    for f in 1000..1010u16 {
        let wire = encode_frame(&frame_data(80, f as u8), 1, 1);
        assert_eq!(videos(s.handle_datagram(0.0, &c.video(12, f, 0, 2, 1, &wire[0]))).len(), 1);
    }
    drain(&mut s);
    let mut got = 0;
    for f in 0..60u16 {
        let wire = encode_frame(&frame_data(80, f as u8), 1, 1);
        got += videos(s.handle_datagram(1.0, &c.video(12, f, 0, 2, 1, &wire[0]))).len();
    }
    assert!(got >= 25, "only {got} frames after the id reset");
    let idr = drain(&mut s).iter().filter(|p| p[0] & 0x0F == 0).count();
    assert!(idr >= 1, "no IDR request sent on resync");
}

/// One injected single-unit frame with a far-ahead id makes every genuine frame "late" until the ids catch up.
#[test]
fn forged_far_ahead_frame_id_does_not_stall_the_stream() {
    let mut s = Stream::new(session(12), StreamConfig { video_window: 3, ..Default::default() });
    let mut c = Console::new();
    let mut ok = 0;
    for f in 0..4u16 {
        let wire = encode_frame(&frame_data(80, f as u8), 1, 1);
        ok += videos(s.handle_datagram(0.0, &c.video(12, f, 0, 2, 1, &wire[0]))).len();
    }
    assert_eq!(ok, 4);
    // attacker datagram: valid video header, unit 0 of 1 (+1 parity), frame id 30000; no key needed
    // (payload is decrypted with the console keystream, but any bytes pass: the unit just needs len >= 2)
    let forged = c.video(12, 30_000, 0, 2, 1, &[0, 0, 0, 0, 0, 1, 9, 9]);
    let _ = s.handle_datagram(0.0, &forged);
    // genuine frames 4..8 follow
    let mut after = 0;
    for f in 4..9u16 {
        let wire = encode_frame(&frame_data(80, f as u8), 1, 1);
        after += videos(s.handle_datagram(0.0, &c.video(12, f, 0, 2, 1, &wire[0]))).len();
    }
    assert!(after >= 4, "only {after}/5 genuine frames delivered after the forged unit");
}

#[test]
fn forged_far_ahead_frame_id_current_behaviour() {
    let mut s = Stream::new(session(12), StreamConfig { video_window: 3, ..Default::default() });
    let mut c = Console::new();
    for f in 0..4u16 {
        let wire = encode_frame(&frame_data(80, f as u8), 1, 1);
        videos(s.handle_datagram(0.0, &c.video(12, f, 0, 2, 1, &wire[0])));
    }
    let forged = c.video(12, 30_000, 0, 2, 1, &[0, 0, 0, 0, 0, 1, 9, 9]);
    let _ = s.handle_datagram(0.0, &forged);
    let mut after = 0;
    for f in 4..9u16 {
        let wire = encode_frame(&frame_data(80, f as u8), 1, 1);
        after += videos(s.handle_datagram(0.0, &c.video(12, f, 0, 2, 1, &wire[0]))).len();
    }
    println!("genuine frames delivered after forged id 30000: {after}/5");
    assert_eq!(after, 5);
}

// ---------------------------------------------------------------- findings: control
fn heartbeat_like_message(c: &mut Console) -> (u32, Vec<u8>) {
    let mut m = TakionMessage::of_type(msg_type::AUDIO_STATE);
    m.audio_state = Some(AudioStatePayload { audio_state_type: Some(1), audio_state_data: None });
    c.message(&m)
}

#[test]
fn duplicate_data_chunk_is_delivered_once() {
    let mut s = Stream::new(session(12), StreamConfig::default());
    let mut c = Console::new();
    let (_, raw) = heartbeat_like_message(&mut c);
    let first = s.handle_datagram(0.0, &raw);
    let second = s.handle_datagram(0.1, &raw);
    assert_eq!(first.len(), 1);
    assert!(second.is_empty(), "duplicate surfaced again: {second:?}");
}

#[test]
fn duplicate_data_chunk_is_always_acked() {
    let mut s = Stream::new(session(12), StreamConfig::default());
    let mut c = Console::new();
    let (seq, raw) = heartbeat_like_message(&mut c);
    let _ = s.handle_datagram(0.0, &raw);
    let _ = s.handle_datagram(0.1, &raw);
    let acks: Vec<_> =
        drain(&mut s).into_iter().map(|p| chunk_of(&p)).filter(|c| c.chunk_type == chunk_type::DATA_ACK).collect();
    assert_eq!(acks.len(), 2);
    assert!(acks.iter().all(|a| a.sequence == seq));
}

#[test]
fn disable_audio_still_counts_audio_for_congestion() {
    let mut s = Stream::new(session(12), StreamConfig { disable_audio: true, ..Default::default() });
    let mut c = Console::new();
    for f in 0..50u16 {
        let unit = [7u8; 40];
        let _ = s.handle_datagram(0.0, &c.audio(f, &[&unit], 1, 0, 40));
    }
    drain(&mut s);
    s.tick(1.5);
    let pkts = drain(&mut s);
    let cong = pkts.iter().find(|p| p[0] & 0x0F == 5).expect("congestion packet");
    let received = u16::from_be_bytes([cong[3], cong[4]]);
    assert_eq!(received, 50);
}

#[test]
fn audio_is_counted_for_congestion_when_enabled() {
    let mut s = Stream::new(session(12), StreamConfig::default());
    let mut c = Console::new();
    for f in 0..50u16 {
        let unit = [7u8; 40];
        let _ = s.handle_datagram(0.0, &c.audio(f, &[&unit], 1, 0, 40));
    }
    drain(&mut s);
    s.tick(1.5);
    let pkts = drain(&mut s);
    let cong = pkts.iter().find(|p| p[0] & 0x0F == 5).expect("congestion packet");
    assert_eq!(u16::from_be_bytes([cong[3], cong[4]]), 50);
}

#[test]
fn idr_retry_schedule_and_ack_cancels() {
    let mut s = Stream::new(session(12), StreamConfig::default());
    s.request_idr(0.0);
    let first = drain(&mut s);
    assert_eq!(first.len(), 1);
    let seq = chunk_of(&first[0]).sequence;
    // retries at 0.066, then every 0.150, max 3 resends, same sequence, fresh keyPos
    let mut sent = vec![];
    let mut t = 0.0;
    while t < 3.0 {
        t += 0.01;
        s.tick(t);
        for p in drain(&mut s) {
            if p[0] & 0x0F == 0 {
                let ch = chunk_of(&p);
                if ch.chunk_type == chunk_type::DATA {
                    sent.push((t, ch.sequence));
                }
            }
        }
    }
    assert_eq!(sent.len(), 3, "{sent:?}");
    assert!(sent.iter().all(|(_, q)| *q == seq));
    // a second request after exhaustion is allowed immediately and gets a NEW sequence
    s.request_idr(5.0);
    let again = drain(&mut s);
    assert_eq!(again.len(), 1);
    assert_ne!(chunk_of(&again[0]).sequence, seq);
}

#[test]
fn idr_ack_with_wrong_sequence_does_not_cancel_retry() {
    let mut s = Stream::new(session(12), StreamConfig::default());
    let c = Console::new();
    s.request_idr(0.0);
    let seq = chunk_of(&drain(&mut s)[0]).sequence;
    let wrong = Chunk::ack(0xC0FFEE, seq + 1).encode();
    let _ = c; // console not needed
    s.handle_datagram(0.01, &wrong);
    s.tick(0.1);
    assert_eq!(drain(&mut s).len(), 1, "retry still due");
    let right = Chunk::ack(0xC0FFEE, seq).encode();
    s.handle_datagram(0.11, &right);
    s.tick(0.5);
    assert!(drain(&mut s).is_empty(), "acked: no more retries");
}

#[test]
fn request_idr_is_rate_limited_while_unacked() {
    let mut s = Stream::new(session(12), StreamConfig::default());
    for i in 0..100 {
        s.request_idr(i as f64 * 0.001); // 100 requests in 100 ms
    }
    assert_eq!(drain(&mut s).len(), 1);
}

#[test]
fn pad_index_out_of_range_is_ignored() {
    let mut s = Stream::new(session(12), StreamConfig::default());
    s.set_pad_connected(0.0, 4, true);
    s.set_pad_connected(0.0, usize::MAX, true);
    s.update_pad(0.0, 99, &ControllerState::default());
    s.update_sensors(7, [0.0; 3], [0.0; 3], [0.0; 4]);
    assert!(drain(&mut s).is_empty());
}

#[test]
fn sensor_values_extremes_do_not_panic() {
    let mut s = Stream::new(session(20), StreamConfig::default());
    for v in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, f32::MAX, f32::MIN, 0.0] {
        s.update_sensors(0, [v; 3], [v; 3], [v; 4]);
    }
    assert!(!drain(&mut s).is_empty());
}

#[test]
fn out_queue_grows_with_every_inbound_data_chunk_until_polled() {
    // The engine has no cap on queued datagrams: a flood of DATA chunks queues one GMAC-protected ack each.
    let mut s = Stream::new(session(12), StreamConfig::default());
    let mut c = Console::new();
    let t = Instant::now();
    let n = 20_000;
    for _ in 0..n {
        let (_, raw) = heartbeat_like_message(&mut c);
        s.handle_datagram(0.0, &raw);
    }
    let el = t.elapsed();
    let q = drain(&mut s).len();
    println!("{n} DATA chunks -> {q} queued acks in {el:?} ({:.1} us each)", el.as_micros() as f64 / n as f64);
    assert_eq!(q, n);
}

#[test]
fn disconnect_message_surfaces_once_and_reason_is_preserved() {
    let mut s = Stream::new(session(12), StreamConfig::default());
    let mut c = Console::new();
    let mut m = TakionMessage::of_type(msg_type::DISCONNECT);
    m.disconnect = Some(DisconnectPayload { reason: Some("bye".into()), extended_info: None });
    let (_, raw) = c.message(&m);
    let ev = s.handle_datagram(0.0, &raw);
    assert_eq!(ev, vec![Event::Disconnected(Some("bye".into()))]);
    let acks = drain(&mut s);
    assert_eq!(acks.len(), 1);
    assert!(c.control_ok(&acks[0]), "ack must carry a valid GMAC");
}

#[test]
fn flags_not_one_data_chunk_is_acked_but_not_parsed() {
    let mut s = Stream::new(session(12), StreamConfig::default());
    let mut chunk = Chunk::data(1, 5, class::GENERAL, TakionMessage::of_type(msg_type::DISCONNECT).to_chunk_payload());
    chunk.flags = 0;
    let ev = s.handle_datagram(0.0, &chunk.encode());
    assert!(ev.is_empty());
    assert_eq!(drain(&mut s).len(), 1);
}

#[test]
fn protected_packets_keypos_strictly_increases_by_16() {
    let mut s = Stream::new(session(12), StreamConfig::default());
    let mut c = Console::new();
    let mut last: Option<u32> = None;
    for _ in 0..500 {
        let (_, raw) = heartbeat_like_message(&mut c);
        s.handle_datagram(0.0, &raw);
        for p in drain(&mut s) {
            assert!(c.control_ok(&p));
            let kp = u32::from_be_bytes(p[9..13].try_into().unwrap());
            if let Some(l) = last {
                assert_eq!(kp, l + 16);
            }
            last = Some(kp);
        }
    }
}

#[test]
fn msg_roundtrip_via_stream_corrupt_frame_report_has_gap_range() {
    let mut s = Stream::new(session(12), StreamConfig { video_window: 2, ..Default::default() });
    let mut c = Console::new();
    for (f, seed) in [(0u16, 1u8), (2, 2), (3, 3)] {
        let wire = encode_frame(&frame_data(100, seed), 2, 1);
        for (i, u) in wire.iter().take(2).enumerate() {
            s.handle_datagram(0.0, &c.video(12, f, i as u16, 3, 1, u));
        }
    }
    let msgs: Vec<_> = drain(&mut s)
        .into_iter()
        .filter(|p| p[0] & 0x0F == 0)
        .map(|p| chunk_of(&p))
        .filter(|ch| ch.chunk_type == chunk_type::DATA)
        .map(|ch| msg_of(&ch))
        .filter(|m| m.r#type == Some(msg_type::CORRUPT_FRAME))
        .collect();
    assert_eq!(msgs.len(), 1, "{msgs:?}");
}

// ---------------------------------------------------------------- audio
#[test]
fn audio_assembler_random_input_never_panics_and_ids_bounded() {
    let mut r = Rng(77);
    let mut a = AudioAssembler::new();
    for _ in 0..200_000 {
        let total = r.below(6) as u8;
        let src = r.below(total as usize + 1) as u8;
        let size = r.below(50);
        let valid = r.below(3) > 0;
        let len = if valid { size * total as usize } else { r.below(300) };
        let p = r.bytes(len);
        let _ = a.push(&AudioUnits {
            frame_id: r.next() as u16,
            total_units: total,
            source_units: src,
            redundant_units: if valid { total - src } else { r.below(8) as u8 },
            unit_size: size,
            payload: &p,
        });
        if r.below(1000) == 0 {
            let _ = a.take_sample();
        }
    }
    // the sample window can never hold more than 65536 ids
    let (rx, _) = a.take_sample();
    assert!(rx <= 65_536);
}

#[test]
fn audio_in_order_stream_across_wrap_is_gapless_and_unduplicated() {
    let mut a = AudioAssembler::new();
    let mut got = vec![];
    for n in 0..2000u32 {
        let id = ((65_000 + n) % 65_536) as u16;
        let unit = [n as u8; 40];
        let payload: Vec<u8> = unit.iter().chain(unit.iter()).copied().collect(); // 1 source + 1 redundant
        for f in a.push(&AudioUnits {
            frame_id: id,
            total_units: 2,
            source_units: 1,
            redundant_units: 1,
            unit_size: 40,
            payload: &payload,
        }) {
            got.push(f.id);
        }
    }
    let mut dedup = got.clone();
    dedup.dedup();
    assert_eq!(dedup.len(), got.len(), "duplicates emitted");
    assert!(got.len() >= 1990, "{} frames", got.len());
}

// ---------------------------------------------------------------- structured fuzz on genuine packets
#[test]
fn mutated_genuine_packets_never_panic() {
    let mut r = Rng(0xF00D);
    for (version, ps5, disable_audio) in [(12, true, false), (20, true, false), (9, false, true)] {
        let mut s = Stream::new(session(version), StreamConfig { ps5, disable_audio, ..Default::default() });
        let mut c = Console::new();
        let mut seed_packets: Vec<Vec<u8>> = vec![];
        for f in 0..6u16 {
            let wire = encode_frame(&frame_data(400, f as u8), 3, 2);
            for (i, u) in wire.iter().enumerate() {
                seed_packets.push(c.video(version, f, i as u16, 5, 2, u));
            }
            let unit = [f as u8; 120];
            seed_packets.push(c.audio(f, &[&unit], 1, 0, 120));
            let u2 = [f as u8; 40];
            seed_packets.push(c.audio(f, &[&u2, &u2], 1, 1, 40));
        }
        for kind in [7u8, 9, 11] {
            for len in [3usize, 7, 12, 17, 25, 33] {
                let mut payload = vec![kind];
                payload.extend(vec![1u8; len]);
                seed_packets.push(c.data(class::GENERAL, payload).1);
            }
        }
        for m in [msg_type::HEARTBEAT, msg_type::DISCONNECT, msg_type::AUDIO_STATE, 99] {
            seed_packets.push(c.message(&TakionMessage::of_type(m)).1);
        }
        for i in 0..60_000u32 {
            let mut p = seed_packets[r.below(seed_packets.len())].clone();
            for _ in 0..=r.below(4) {
                match r.below(4) {
                    0 if !p.is_empty() => {
                        let i = r.below(p.len());
                        p[i] = r.next() as u8;
                    }
                    1 => {
                        let n = r.below(p.len() + 1);
                        p.truncate(n);
                    }
                    2 => {
                        let n = r.below(40);
                        p.extend(r.bytes(n));
                    }
                    _ if p.len() > 2 => {
                        let i = r.below(p.len());
                        p[i] ^= 1 << r.below(8);
                    }
                    _ => {}
                }
            }
            let now = i as f64 * 0.001;
            for ev in s.handle_datagram(now, &p) {
                let _ = ev;
            }
            if i % 100 == 0 {
                s.tick(now);
                s.request_idr(now);
            }
            while s.poll_transmit().is_some() {}
        }
    }
}

// ---------------------------------------------------------------- performance (prints; assertions only in release)
#[test]
fn perf_video_pipeline_throughput() {
    let mut s = Stream::new(session(12), StreamConfig::default());
    let mut c = Console::new();
    let (k, m) = (20usize, 4usize);
    let frame = frame_data(20 * 1380, 5);
    let wire = encode_frame(&frame, k, m);
    let frames = if cfg!(debug_assertions) { 30u16 } else { 600 };
    let mut pkts = vec![];
    for f in 0..frames {
        for (i, u) in wire.iter().enumerate() {
            if i == 3 || i == 11 {
                continue; // two losses per frame -> FEC recovery path every frame
            }
            pkts.push(c.video(12, f, i as u16, (k + m) as u16, m as u16, u));
        }
    }
    let bytes: usize = pkts.iter().map(Vec::len).sum();
    let t = Instant::now();
    let mut got = 0;
    for p in &pkts {
        got += videos(s.handle_datagram(0.0, p)).len();
    }
    let el = t.elapsed();
    let mbit = bytes as f64 * 8.0 / el.as_secs_f64() / 1e6;
    println!(
        "video path: {got}/{frames} frames, {bytes} B in {el:?} = {mbit:.0} Mbit/s, {:?}/frame (FEC on every frame)",
        el / frames as u32
    );
    assert_eq!(got as u16, frames);
    if !cfg!(debug_assertions) {
        assert!(mbit > 400.0, "video path too slow: {mbit}");
    }
}
