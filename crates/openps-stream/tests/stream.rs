//! Stream engine against a mock console. The console is written from the same spec, so these prove internal
//! consistency (framing, decrypt offsets incl. keyPos wrap, FEC path, GMAC/encryption symmetry, timers), not
//! wire compatibility with a real PlayStation.
use openps_input::state::{Button, ControllerState};
use openps_stream::audio::OpusFrame;
use openps_stream::padblob::PadOutput;
use openps_stream::{Event, Stream, StreamConfig};
use openps_takion::chunk::{chunk_type, class, Chunk};
use openps_takion::proto::*;
use openps_testkit::stream::{
    chunk_of, decrypt_feedback, encode_frame, frame_data, msg_of, session, StreamConsole as Console,
};

fn drain(s: &mut Stream) -> Vec<Vec<u8>> {
    std::iter::from_fn(|| s.poll_transmit()).collect()
}

fn videos(evs: Vec<Event>) -> Vec<(u16, Vec<u8>)> {
    evs.into_iter().filter_map(|e| if let Event::Video(f) = e { Some((f.id, f.data)) } else { None }).collect()
}

// ---- video ------------------------------------------------------------------------------------------------------------
#[test]
fn encrypted_video_units_reassemble_into_frames() {
    for version in [9, 20] {
        let mut s = Stream::new(session(version), StreamConfig::default());
        let mut c = Console::new();
        let data = frame_data(900, 3);
        let wire = encode_frame(&data, 4, 2);
        let mut got = vec![];
        for (i, u) in wire.iter().enumerate() {
            got.extend(videos(s.handle_datagram(0.0, &c.video(version, 0, i as u16, 6, 2, u))));
        }
        assert_eq!(got, vec![(0, data)], "version {version}");
    }
}

#[test]
fn lost_units_are_recovered_with_fec_through_the_full_path() {
    let mut s = Stream::new(session(12), StreamConfig::default());
    let mut c = Console::new();
    let data = frame_data(1200, 8);
    let wire = encode_frame(&data, 5, 2);
    let mut got = vec![];
    for (i, u) in wire.iter().enumerate() {
        if i == 1 || i == 3 {
            continue;
        } // two source units lost
        got.extend(videos(s.handle_datagram(0.0, &c.video(12, 0, i as u16, 7, 2, u))));
    }
    assert_eq!(got, vec![(0, data)]);
}

#[test]
fn video_keypos_wrap_is_decrypted_correctly() {
    let mut s = Stream::new(session(12), StreamConfig::default());
    let mut c = Console::new();
    c.pos = 0xFFFF_FFC0; // a few packets before the u32 wrap
    let mut ids = vec![];
    for f in 0..6u16 {
        let data = frame_data(100, f as u8);
        let wire = encode_frame(&data, 2, 1);
        for (i, u) in wire.iter().take(2).enumerate() {
            for (id, d) in videos(s.handle_datagram(0.0, &c.video(12, f, i as u16, 3, 1, u))) {
                assert_eq!(d, frame_data(100, id as u8));
                ids.push(id);
            }
        }
    }
    assert_eq!(ids, vec![0, 1, 2, 3, 4, 5]);
    assert!(c.pos > 0x1_0000_0000); // the console really crossed 2^32
}

#[test]
fn a_skipped_frame_triggers_a_protected_corrupt_frame_report() {
    let mut s = Stream::new(session(12), StreamConfig { video_window: 2, ..Default::default() });
    let mut c = Console::new();
    for (f, seed) in [(0u16, 1u8), (2, 2), (3, 3)] {
        // frame 1 never arrives
        let wire = encode_frame(&frame_data(100, seed), 2, 1);
        for (i, u) in wire.iter().enumerate() {
            s.handle_datagram(0.0, &c.video(12, f, i as u16, 3, 1, u));
        }
    }
    let out = drain(&mut s);
    let report =
        out.iter().map(|p| chunk_of(p)).find(|ch| ch.class == class::CORRUPT_FRAME).expect("corrupt frame report");
    assert!(c.control_ok(&out.iter().find(|p| chunk_of(p).class == class::CORRUPT_FRAME).unwrap().clone()));
    let m = msg_of(&report);
    assert_eq!(m.r#type, Some(msg_type::CORRUPT_FRAME));
    let cp = m.corrupt.unwrap();
    assert_eq!((cp.start, cp.end), (Some(1), Some(4)));
}

// ---- control ------------------------------------------------------------------------------------------------------------
#[test]
fn every_data_chunk_is_acked_and_heartbeats_are_answered() {
    let mut s = Stream::new(session(12), StreamConfig::default());
    let mut c = Console::new();
    let (seq, pkt) = c.message(&TakionMessage::of_type(msg_type::HEARTBEAT));
    assert!(s.handle_datagram(0.0, &pkt).is_empty());
    let out = drain(&mut s);
    assert_eq!(out.len(), 2);
    let ack = chunk_of(&out[0]);
    assert_eq!((ack.chunk_type, ack.sequence, ack.class), (chunk_type::DATA_ACK, seq, class::CONTROL));
    let hb = chunk_of(&out[1]);
    assert_eq!((hb.chunk_type, hb.flags, hb.class), (chunk_type::DATA, 1, class::GENERAL));
    assert_eq!((hb.payload_kind, hb.payload.as_slice()), (Some(0), &[0x08, 0x03][..]));
    assert!(out.iter().all(|p| c.control_ok(p)));
    assert_eq!(hb.sequence, 0x4827); // continues the handshake's TSN sequence
}

#[test]
fn disconnect_message_surfaces_with_its_reason() {
    let mut s = Stream::new(session(12), StreamConfig::default());
    let mut c = Console::new();
    let mut m = TakionMessage::of_type(msg_type::DISCONNECT);
    m.disconnect = Some(DisconnectPayload { reason: Some("bye".into()), extended_info: None });
    let (_, pkt) = c.message(&m);
    assert_eq!(s.handle_datagram(0.0, &pkt), vec![Event::Disconnected(Some("bye".into()))]);
}

#[test]
fn other_messages_and_pad_blobs_are_surfaced() {
    let mut s = Stream::new(session(12), StreamConfig::default());
    let mut c = Console::new();
    let mut m = TakionMessage::of_type(msg_type::STREAM_INFO);
    m.timer = Some(TimerPayload { seconds_left: Some(30), total_seconds: Some(60) });
    let (_, pkt) = c.message(&m);
    let ev = s.handle_datagram(0.0, &pkt);
    assert!(matches!(&ev[0], Event::Message(b) if b.timer.as_ref().unwrap().seconds_left == Some(30)));
    let (_, rumble) = c.data(class::GENERAL, vec![7, 1, 200, 40]);
    assert_eq!(
        s.handle_datagram(0.0, &rumble),
        vec![Event::PadOutput(PadOutput::Rumble { pad: 1, left: 200, right: 40 })]
    );
}

#[test]
fn idr_request_is_rate_limited_retried_and_cancelled_by_its_ack() {
    let mut s = Stream::new(session(12), StreamConfig::default());
    let mut c = Console::new();
    s.request_idr(0.0);
    let first = drain(&mut s);
    assert_eq!(first.len(), 1);
    let seq = chunk_of(&first[0]).sequence;
    assert_eq!(msg_of(&chunk_of(&first[0])).r#type, Some(msg_type::IDR_REQUEST));
    s.request_idr(0.05); // < 150 ms and unacked: ignored
    assert!(drain(&mut s).is_empty());
    s.tick(0.05); // before the first retry at 66 ms
    assert!(drain(&mut s).is_empty());
    s.tick(0.07); // first retry
    let r = drain(&mut s);
    assert_eq!(r.len(), 1);
    assert_eq!(chunk_of(&r[0]).sequence, seq); // same sequence, new keyPos
    assert!(c.control_ok(&r[0]));
    s.tick(0.23);
    s.tick(0.40); // two more, 150 ms after each previous retry
    assert_eq!(drain(&mut s).len(), 2);
    s.tick(0.52); // 3 resends max: nothing further
    assert!(drain(&mut s).is_empty());

    // an ack cancels pending retries
    let mut s = Stream::new(session(12), StreamConfig::default());
    s.request_idr(0.0);
    let seq = chunk_of(&drain(&mut s)[0]).sequence;
    s.handle_datagram(0.01, &Chunk::new(0, chunk_type::DATA_ACK, 0, seq, class::CONTROL, vec![0; 4]).encode());
    s.tick(0.5);
    assert!(drain(&mut s).is_empty());
    s.request_idr(0.6); // acked, so a new request is allowed immediately
    assert_eq!(drain(&mut s).len(), 1);
}

#[test]
fn pad_connection_is_announced_with_type_6_and_retried_until_acked() {
    let mut s = Stream::new(session(12), StreamConfig::default());
    s.set_pad_connected(0.0, 1, true);
    let out = drain(&mut s);
    let m = msg_of(&chunk_of(&out[0]));
    assert_eq!(m.r#type, Some(msg_type::CONTROLLER_CONNECTION));
    let cc = m.controller_connection.unwrap();
    assert_eq!((cc.controller_id, cc.connected, cc.controller_type), (Some(1), Some(true), Some(6)));
    s.tick(0.07);
    let out = drain(&mut s);
    assert_eq!(out.iter().filter(|p| p[0] & 0x0F == 0).count(), 1); // the retry (the history packet with the presence record is separate)
    s.set_pad_connected(0.1, 1, false);
    let m = msg_of(&chunk_of(&drain(&mut s).remove(0)));
    assert_eq!(m.controller_connection.unwrap().controller_type, Some(0));
}

#[test]
fn a_pad_join_is_retried_about_fifteen_times_while_an_idr_request_gives_up_after_three() {
    // PXPlay (ee$2) retries the controller-connection message every 150 ms and gives up at 15 (about 2.3 s); chiaki-ng resends DATA for
    // about 5 s. A join lost to one Wi-Fi hiccup must not leave the pad ignored by the console for the whole session.
    let mut s = Stream::new(session(12), StreamConfig::default());
    s.set_pad_connected(0.0, 1, true);
    drain(&mut s);
    let (mut resends, mut t) = (0, 0.07);
    while t < 6.0 {
        s.tick(t);
        resends += drain(&mut s).iter().filter(|p| p[0] & 0x0F == 0).count();
        t += 0.15;
    }
    assert_eq!(resends, 15);

    let mut s = Stream::new(session(12), StreamConfig::default());
    s.request_idr(0.0);
    drain(&mut s);
    let (mut resends, mut t) = (0, 0.07);
    while t < 6.0 {
        s.tick(t);
        resends += drain(&mut s).iter().filter(|p| p[0] & 0x0F == 0).count();
        t += 0.15;
    }
    assert_eq!(resends, 3, "IDR retries are unchanged");
}

#[test]
fn disconnect_sends_three_messages() {
    let mut s = Stream::new(session(12), StreamConfig::default());
    s.disconnect();
    let out = drain(&mut s);
    assert_eq!(out.len(), 3);
    for p in &out {
        let m = msg_of(&chunk_of(p));
        assert_eq!(
            (m.r#type, m.disconnect.unwrap().reason.as_deref()),
            (Some(msg_type::DISCONNECT), Some("Client Disconnecting"))
        );
    }
}

// ---- audio / haptics ---------------------------------------------------------------------------------------------------
#[test]
fn encrypted_audio_yields_opus_frames_in_order() {
    let mut s = Stream::new(session(12), StreamConfig { ps5: false, ..Default::default() });
    let mut c = Console::new();
    let u0 = [1u8; 40];
    let u1 = [2u8; 40];
    assert_eq!(
        s.handle_datagram(0.0, &c.audio(100, &[&u0], 1, 0, 40)),
        vec![Event::Audio(vec![OpusFrame { id: 100, data: u0.to_vec() }])]
    );
    assert_eq!(
        s.handle_datagram(0.0, &c.audio(101, &[&u1], 1, 0, 40)),
        vec![Event::Audio(vec![OpusFrame { id: 101, data: u1.to_vec() }])]
    );
    assert!(s.handle_datagram(0.0, &c.audio(101, &[&u1], 1, 0, 40)).is_empty());
    // repeat is stale
}

#[test]
fn ps5_large_units_are_haptics_and_use_their_own_decrypt_counter() {
    let mut s = Stream::new(session(12), StreamConfig::default());
    let mut c = Console::new();
    let h = [9u8; 120];
    let ev = s.handle_datagram(0.0, &c.audio(7, &[&h], 1, 0, 120));
    assert_eq!(ev, vec![Event::Haptic(h)]);
    // the same packet size on a PS4 is just audio (and 120 B is not a valid Opus unit, but it is delivered as frames)
    let mut s4 = Stream::new(session(12), StreamConfig { ps5: false, ..Default::default() });
    let mut c4 = Console::new();
    assert!(matches!(s4.handle_datagram(0.0, &c4.audio(7, &[&h], 1, 0, 120))[0], Event::Audio(_)));
}

#[test]
fn disabled_audio_is_not_decoded() {
    let mut s = Stream::new(session(12), StreamConfig { disable_audio: true, ps5: false, ..Default::default() });
    let mut c = Console::new();
    assert!(s.handle_datagram(0.0, &c.audio(1, &[&[1u8; 40]], 1, 0, 40)).is_empty());
}

// ---- controller feedback ------------------------------------------------------------------------------------------------
#[test]
fn button_press_sends_encrypted_history_and_sticks_send_state() {
    for version in [9, 20] {
        let mut s = Stream::new(session(version), StreamConfig::default());
        let mut c = Console::new();
        let t20 = version >= 20;
        let mut st = ControllerState::default();
        st.buttons.insert(Button::CROSS);
        st.left_x = 1.0;
        s.update_pad(0.0, 0, &st);
        let out = drain(&mut s);
        assert_eq!(out.len(), 2, "state + history");
        let (kind, seq, state) = decrypt_feedback(&mut c, &out[0], t20);
        assert_eq!((kind, seq), (6, 1537));
        assert_eq!(state.len(), if t20 { 28 } else { 25 });
        assert_eq!(&state[17..19], &32767i16.to_be_bytes());
        let (kind, seq, hist) = decrypt_feedback(&mut c, &out[1], t20);
        assert_eq!((kind, seq), (1, 0));
        assert_eq!(hist, vec![0x80, 0x88, 0xFF]);
    }
}

#[test]
fn history_is_resent_every_100ms_and_sequences_increase() {
    let mut s = Stream::new(session(12), StreamConfig::default());
    let mut c = Console::new();
    let mut st = ControllerState::default();
    st.buttons.insert(Button::CIRCLE);
    s.update_pad(0.0, 0, &st);
    drain(&mut s);
    s.tick(0.05);
    assert!(drain(&mut s).is_empty());
    s.tick(0.11);
    let out = drain(&mut s);
    assert_eq!(out.len(), 2, "the state packet is repeated too");
    let (kind, _, _) = decrypt_feedback(&mut c, &out[0], false);
    assert_eq!(kind, 6);
    let (kind, seq, hist) = decrypt_feedback(&mut c, &out[1], false);
    assert_eq!((kind, seq, hist), (1, 1, vec![0x80, 0x89, 0xFF]));
}

#[test]
fn lt_release_does_not_clear_or_stick_when_rt_is_pressed_first() {
    let mut stream = Stream::new(session(12), StreamConfig::default());
    let mut console = Console::new();
    let mut state = ControllerState { r2: 1.0, ..Default::default() };

    stream.update_pad(0.0, 0, &state);
    drain(&mut stream);

    state.l2 = 1.0;
    stream.update_pad(0.01, 0, &state);
    let out = drain(&mut stream);
    let (_, _, both_pressed) = decrypt_feedback(&mut console, &out[0], false);
    let (both_pressed, _) = both_pressed.as_chunks::<3>();
    assert!(both_pressed.iter().any(|r| r[1] == 0x86 && r[2] == 0xFF));
    assert!(both_pressed.iter().any(|r| r[1] == 0x87 && r[2] == 0xFF));

    state.l2 = 0.0;
    stream.update_pad(0.02, 0, &state);
    let out = drain(&mut stream);
    let (_, _, lt_released) = decrypt_feedback(&mut console, &out[0], false);
    let (lt_released, _) = lt_released.as_chunks::<3>();
    assert!(lt_released.iter().any(|r| r[1] == 0x86 && r[2] == 0));
    assert!(lt_released.iter().any(|r| r[1] == 0x87 && r[2] == 0xFF));

    state.r2 = 0.0;
    stream.update_pad(0.03, 0, &state);
    let out = drain(&mut stream);
    let (_, _, both_released) = decrypt_feedback(&mut console, &out[0], false);
    let (both_released, _) = both_released.as_chunks::<3>();
    assert!(both_released.iter().any(|r| r[1] == 0x86 && r[2] == 0));
    assert!(both_released.iter().any(|r| r[1] == 0x87 && r[2] == 0));
}

#[test]
fn the_state_packet_is_repeated_while_idle_so_a_lost_one_cannot_stick_a_stick() {
    let mut s = Stream::new(session(12), StreamConfig::default());
    let st = ControllerState { right_x: 0.5, ..Default::default() };
    s.update_pad(0.0, 0, &st);
    drain(&mut s);
    s.tick(0.05);
    assert!(drain(&mut s).is_empty());
    s.tick(0.11);
    assert_eq!(drain(&mut s).len(), 1);
    s.tick(0.15);
    assert!(drain(&mut s).is_empty(), "then every 100 ms, not every tick");
}

#[test]
fn lt_and_rt_changing_together_are_sent_as_one_history_packet_each() {
    // Reproduces the burnout bug: both triggers change in the same controller sample (macOS reports the whole pad at once). The
    // console applies one new event per packet, so a single batched packet left LT or RT unapplied (a stuck LT after release).
    let mut stream = Stream::new(session(12), StreamConfig::default());
    let mut console = Console::new();
    let both = ControllerState { l2: 1.0, r2: 1.0, ..Default::default() };
    stream.update_pad(0.0, 0, &both);
    let out = drain(&mut stream);
    assert_eq!(out.len(), 2, "one history packet per trigger");
    let (k0, seq0, first) = decrypt_feedback(&mut console, &out[0], false);
    let (k1, seq1, second) = decrypt_feedback(&mut console, &out[1], false);
    assert_eq!((k0, k1, seq1), (1, 1, seq0 + 1), "consecutive history sequences");
    assert_eq!(first, vec![0x80, 0x86, 0xFF], "LT alone");
    assert_eq!(second, vec![0x80, 0x87, 0xFF, 0x80, 0x86, 0xFF], "RT in front, LT behind it");

    stream.update_pad(0.02, 0, &ControllerState::default());
    let out = drain(&mut stream);
    assert_eq!(out.len(), 2, "each release is its own packet");
    let (_, _, lt_release) = decrypt_feedback(&mut console, &out[0], false);
    let (_, _, rt_release) = decrypt_feedback(&mut console, &out[1], false);
    assert_eq!(&lt_release[..3], &[0x80, 0x86, 0x00], "the LT release can no longer be dropped behind the RT release");
    assert_eq!(&rt_release[..3], &[0x80, 0x87, 0x00]);
}

#[test]
fn extra_pads_are_stamped_and_announced_in_history() {
    let mut s = Stream::new(session(12), StreamConfig::default());
    let mut c = Console::new();
    s.set_pad_connected(0.0, 2, true);
    drain(&mut s);
    let mut st = ControllerState::default();
    st.buttons.insert(Button::TRIANGLE);
    s.update_pad(0.01, 2, &st);
    let out = drain(&mut s);
    let (_, _, hist) = decrypt_feedback(&mut c, out.last().unwrap(), false);
    // newest first: the triangle press, then the presence record queued by set_pad_connected, both stamped with pad 2
    assert_eq!(&hist[..3], &[0x82, 0x8B, 0xFF]);
    assert_eq!(&hist[3..5], &[0x82, 0xBF]);
}

// ---- congestion ----------------------------------------------------------------------------------------------------------------
#[test]
fn congestion_report_goes_out_each_second_with_the_spec_ratio_rule() {
    let mut s = Stream::new(session(12), StreamConfig { ps5: false, ..Default::default() });
    let mut c = Console::new();
    // second 1: one complete frame, no loss reported yet (ratio 0 for the first sample)
    for (i, u) in encode_frame(&frame_data(100, 1), 2, 1).iter().enumerate() {
        s.handle_datagram(0.0, &c.video(12, 0, i as u16, 3, 1, u));
    }
    s.tick(0.5);
    assert!(drain(&mut s).iter().all(|p| chunk_of(p).chunk_type != 99)); // nothing yet
    s.tick(1.2);
    let out = drain(&mut s);
    let cong = out.iter().find(|p| p[0] & 0x0F == 5).expect("congestion packet");
    assert_eq!(cong.len(), 15);
    assert_eq!(&cong[3..5], &2u16.to_be_bytes()); // 2 units received (frame done after 2 of 3 units, 3rd counted as late/stored)
    assert_eq!(&cong[5..7], &0u16.to_be_bytes());
    assert!(c.gmac_ok_zeroed(cong, 7, 11));
}

#[test]
fn congestion_can_be_disabled() {
    let mut s = Stream::new(session(12), StreamConfig { congestion: false, ..Default::default() });
    let mut c = Console::new();
    s.handle_datagram(0.0, &c.data(class::GENERAL, TakionMessage::of_type(msg_type::HEARTBEAT).to_chunk_payload()).1);
    drain(&mut s);
    s.tick(5.0);
    assert!(drain(&mut s).is_empty());
}

#[test]
fn garbage_is_ignored() {
    let mut s = Stream::new(session(12), StreamConfig::default());
    assert!(s.handle_datagram(0.0, &[]).is_empty());
    assert!(s.handle_datagram(0.0, &[2, 1, 2]).is_empty()); // short video
    assert!(s.handle_datagram(0.0, &[3, 1]).is_empty()); // short audio
    assert!(s.handle_datagram(0.0, &[0x0F; 40]).is_empty()); // unknown type
    assert!(drain(&mut s).is_empty());
}
