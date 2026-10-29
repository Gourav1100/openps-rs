//! Senkusha against a simulated console on a virtual clock. As elsewhere the console is written from the same
//! spec, so this proves the state machine's behaviour (searches, timeouts, retries), not wire compatibility.
use openps_takion::senkusha::*;
use openps_testkit::senkusha::SenkushaConsole;

/// Drive `s` against `con`. `rtt_s` is the simulated round trip: a reply is delivered that much later.
fn run(s: &mut Senkusha, con: &mut SenkushaConsole, rtt_s: f64) -> f64 {
    let mut now = 0.0;
    for _ in 0..200_000 {
        let mut progressed = false;
        while let Some(p) = s.poll_transmit(now) {
            progressed = true;
            let replies = con.on_packet(&p);
            if !replies.is_empty() {
                now += rtt_s;
                for r in replies {
                    s.handle_datagram(now, &r);
                }
            }
        }
        if !matches!(s.state(), State::Running) {
            return now;
        }
        if !progressed {
            match s.poll_timeout() {
                Some(t) => {
                    now = now.max(t);
                    s.handle_timeout(now);
                }
                None => return now,
            }
        }
    }
    panic!("did not settle");
}

fn senkusha(ps5: bool) -> Senkusha {
    let mut n = 0u32;
    Senkusha::new(SenkushaConfig { ps5, ..Default::default() }, move || {
        n = n.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        n
    })
}

#[test]
fn ps5_full_run_measures_rtt_and_both_mtus() {
    let mut s = senkusha(true);
    let mut con = SenkushaConsole::new(true, 1300, 1200);
    run(&mut s, &mut con, 0.0045);
    let r = s.result().cloned().unwrap_or_else(|| panic!("{:?}", s.state()));
    assert_eq!(r.rtt_ms, 4); // 4.5 ms floors to 4
    assert!((1298..=1300).contains(&r.mtu_in), "{r:?}"); // the original's loop cannot confirm the very last value
    assert!(r.mtu_out <= r.mtu_in && (1198..=1200).contains(&r.mtu_out), "{r:?}");
    // protocol request only on PS5; echo on/off; both client-MTU reports; goodbye
    assert_eq!(con.log.first().map(String::as_str), Some("init"));
    assert!(con.log.contains(&"protocol-request".to_string()) && con.log.contains(&"big".to_string()));
    let pos = |needle: &str| {
        con.log.iter().position(|l| l == needle).unwrap_or_else(|| panic!("{needle} missing in {:?}", con.log))
    };
    assert!(
        pos("echo:true") < pos("echo:false")
            && pos("echo:false") < pos("client-mtu:true")
            && pos("client-mtu:true") < pos("client-mtu:false")
    );
    assert!(con.disconnected);
    assert_eq!(con.client_mtu_commands.first(), Some(&(r.mtu_in, r.mtu_in, true)));
    assert_eq!(con.client_mtu_commands.last(), Some(&(r.mtu_out, r.mtu_in, false)));
}

#[test]
fn rtt_probes_are_548_bytes_and_mtu_probes_follow_the_candidate_sizes() {
    let mut s = senkusha(true);
    let mut con = SenkushaConsole::new(true, 1400, 1300);
    run(&mut s, &mut con, 0.002);
    assert!(s.result().is_some());
    assert!(con.probe_sizes[..RTT_PROBES as usize].iter().all(|n| *n == RTT_PROBE_LEN));
    let up = &con.probe_sizes[RTT_PROBES as usize..];
    assert!(!up.is_empty() && up.iter().all(|n| (576 - 28..=1454 - 28).contains(n)), "{up:?}");
    // the downlink search starts at the maximum and every request is within range
    assert_eq!(con.mtu_requests[0], 1454);
    assert!(con.mtu_requests.iter().all(|m| (576..=1454).contains(&(*m as i32))));
}

#[test]
fn ps4_skips_the_protocol_request() {
    let mut s = senkusha(false);
    let mut con = SenkushaConsole::new(false, 1454, 1454);
    run(&mut s, &mut con, 0.001);
    let r = s.result().unwrap();
    assert!(!con.log.contains(&"protocol-request".to_string()));
    assert_eq!((r.mtu_in, r.mtu_out), (1454, 1454)); // the maximum works, so the search ends there
    assert_eq!(r.rtt_ms, 1);
}

#[test]
fn a_downlink_that_never_passes_a_probe_leaves_the_originals_default_of_1454() {
    // Documented quirk: the best-known downlink size starts at 1454 and is only ever replaced on success, so when
    // every probe fails the result is still 1454 and the stream proceeds with the default MTU. The uplink path here
    // is the smallest the 548-byte echo probes (576 on the wire) can cross.
    let mut s = senkusha(true);
    let mut con = SenkushaConsole::new(true, 500, 600);
    run(&mut s, &mut con, 0.002);
    let r = s.result().cloned().unwrap_or_else(|| panic!("{:?}", s.state()));
    assert_eq!(r.mtu_in, 1454);
    assert!((598..=600).contains(&r.mtu_out), "{r:?}");
}

#[test]
fn an_uplink_too_small_for_the_echo_probes_fails_the_test() {
    // The RTT probes are 548 bytes (576 on the wire); a path that cannot carry them never answers.
    let mut s = senkusha(true);
    let mut con = SenkushaConsole::new(true, 1454, 500);
    run(&mut s, &mut con, 0.002);
    assert!(matches!(s.state(), State::Failed(_)), "{:?}", s.state());
}

#[test]
fn lost_echo_probes_are_resent_and_do_not_fail_the_test() {
    let mut s = senkusha(true);
    let mut con = SenkushaConsole::new(true, 1454, 1454);
    con.drop_first_probes = 6;
    run(&mut s, &mut con, 0.003);
    assert!(s.result().is_some(), "{:?}", s.state());
}

#[test]
fn a_console_that_never_echoes_fails_after_three_attempts() {
    let mut s = senkusha(true);
    let mut con = SenkushaConsole::new(true, 1454, 1454);
    con.drop_first_probes = usize::MAX;
    run(&mut s, &mut con, 0.001);
    match s.state() {
        State::Failed(why) => assert!(why.contains("after 3 attempts"), "{why}"),
        st => panic!("{st:?}"),
    }
    assert_eq!(con.log.iter().filter(|l| *l == "init").count(), 3); // a fresh attempt each time
    assert!(con.disconnected); // and a goodbye each time
}

#[test]
fn a_refused_bang_fails_the_attempt() {
    let mut s = senkusha(true);
    let mut con = SenkushaConsole::new(true, 1454, 1454);
    con.refuse_bang = true;
    run(&mut s, &mut con, 0.001);
    assert!(matches!(s.state(), State::Failed(_)), "{:?}", s.state());
}

#[test]
fn a_silent_network_gives_up_with_bounded_traffic() {
    let mut s = senkusha(true);
    let (mut sent, mut now) = (0, 0.0);
    for _ in 0..1000 {
        while s.poll_transmit(now).is_some() {
            sent += 1;
        }
        if !matches!(s.state(), State::Running) {
            break;
        }
        match s.poll_timeout() {
            Some(t) => {
                now = now.max(t);
                s.handle_timeout(now);
            }
            None => break,
        }
    }
    assert!(matches!(s.state(), State::Failed(_)));
    assert_eq!(sent, 12); // 3 attempts x (INIT + 3 resends)
    assert!(now < 2.0, "{now}"); // 100 ms waits, not the handshake's 1 s
}

#[test]
fn garbage_and_stray_datagrams_are_ignored() {
    let mut s = senkusha(true);
    let mut con = SenkushaConsole::new(true, 1400, 1300);
    let mut x = 12345u64;
    // interleave junk with the real exchange
    let mut now = 0.0;
    for step in 0..100_000 {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        if step % 3 == 0 {
            let junk: Vec<u8> = (0..(x % 90) as usize).map(|i| (x >> (i % 56)) as u8).collect();
            s.handle_datagram(now, &junk);
        }
        let mut progressed = false;
        while let Some(p) = s.poll_transmit(now) {
            progressed = true;
            for r in con.on_packet(&p) {
                now += 0.002;
                s.handle_datagram(now, &r);
            }
        }
        if !matches!(s.state(), State::Running) {
            break;
        }
        if !progressed {
            if let Some(t) = s.poll_timeout() {
                now = now.max(t);
                s.handle_timeout(now);
            }
        }
    }
    assert!(s.result().is_some(), "{:?}", s.state());
}
