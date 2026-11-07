//! Writes output reports to DualSense pads on a thread of its own. The pad is opened shared, next to GameController (which keeps
//! reading the input). Writes happen on change only; the newest request per pad wins.

use crate::backoff::Backoff;
use crate::haptic_rumble::HapticRumble;
use crate::haptics::{pair_report, setup_report, silence_report, single_report, HapticStream, Out, PERIOD_NS};
use crate::report::{build, firmware_version, trigger_reduction, Bus, OutState, Probe, Tuning};
use hidapi::{BusType, HidApi, HidDevice};
use openps_stream::padblob::PadOutput;
use std::collections::VecDeque;
use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

const SONY: u16 = 0x054C;
const DUALSENSE: u16 = 0x0CE6;
const DUALSENSE_EDGE: u16 = 0x0DF2;
/// After the pad cannot be found or opened, wait this long before looking again (a device-list refresh is expensive and every
/// command, up to 100 haptic frames a second, would otherwise repeat it and its warning).
const RETRY_AFTER: Duration = Duration::from_secs(1);

enum Cmd {
    Apply(usize, PadOutput),
    Haptic(usize, [u8; 120]),
    /// Rumble and triggers off (the pad is no longer the one in play); the remembered effects come back on `false`.
    Quiet(usize, bool),
    /// A session ended: forget rumble and trigger effects (the lightbar stays).
    Clear(usize),
    Close(usize),
    /// The user's rumble and trigger strength, for every pad.
    Tune(Tuning),
    /// A Test button: make the pad rumble or resist for a moment, whatever the console and the pad's mode say.
    Probe(usize, Probe),
}

#[derive(Clone)]
pub struct HidOut {
    tx: Sender<Cmd>,
}

struct Slot {
    state: OutState,
    quiet: bool,
    dev: Option<(HidDevice, Bus)>,
    /// The 4-bit sequence shared by every Bluetooth output report (state, coil setup, haptics).
    seq: u8,
    /// Haptic blocks sent so far, wrapping: the pad's audio counter.
    counter: u8,
    dirty: bool,
    haptics: VecDeque<[u8; 120]>,
    /// USB only: the console's haptic audio played as rumble, since the voice coils need the pad's USB audio device.
    haptic_rumble: HapticRumble,
    /// Bluetooth only: the console's haptics paced out as voice-coil audio.
    stream: HapticStream,
    /// The coil setup report has been accepted since the pad was opened.
    setup_ok: bool,
    /// When the next block is due, while the stream wants one.
    next_tick: Option<Instant>,
    /// A Test button is running: what it does and when it ends.
    probe: Option<(Probe, Instant)>,
    /// Console rumble is being held back because haptics are reaching the pad (PXPlay does the same).
    gated: bool,
    warned_missing_device: bool,
    retry: Backoff,
}

impl Slot {
    fn new(tuning: Tuning) -> Self {
        Self {
            state: OutState { tuning, ..OutState::default() },
            probe: None,
            quiet: false,
            dev: None,
            seq: 0,
            counter: 0,
            dirty: false,
            haptics: VecDeque::new(),
            haptic_rumble: HapticRumble::default(),
            stream: HapticStream::new(true),
            setup_ok: false,
            next_tick: None,
            gated: false,
            warned_missing_device: false,
            retry: Backoff::default(),
        }
    }
}

impl HidOut {
    pub fn start() -> Self {
        let (tx, rx) = channel::<Cmd>();
        std::thread::Builder::new()
            .name("pad-hid".into())
            .spawn(move || {
                let mut api = match HidApi::new() {
                    Ok(api) => api,
                    Err(e) => {
                        log::error!("cannot initialize HID output for controllers: {e}");
                        return;
                    }
                };
                let mut slots: Vec<Slot> = vec![];
                let mut tuning = Tuning::default();
                loop {
                    // Wake for the earliest deadline: a rumble-from-haptics expiry (the motors must stop when the audio does) or the next
                    // haptic block.
                    let wake = slots
                        .iter()
                        .flat_map(|s| [s.haptic_rumble.deadline(), s.next_tick, s.probe.map(|(_, end)| end)])
                        .flatten()
                        .min();
                    let first = match wake {
                        None => match rx.recv() {
                            Ok(cmd) => Some(cmd),
                            Err(_) => break,
                        },
                        Some(at) => match rx.recv_timeout(at.saturating_duration_since(Instant::now())) {
                            Ok(cmd) => Some(cmd),
                            Err(RecvTimeoutError::Timeout) => None,
                            Err(RecvTimeoutError::Disconnected) => break,
                        },
                    };
                    let mut dirty = vec![];
                    for cmd in first.into_iter().chain(std::iter::from_fn(|| rx.try_recv().ok())) {
                        if let Cmd::Tune(t) = &cmd {
                            tuning = *t;
                            for (rank, slot) in slots.iter_mut().enumerate() {
                                slot.state.tuning = *t;
                                slot.dirty = true;
                                dirty.push(rank);
                            }
                            continue;
                        }
                        if let Some(rank) = handle(&mut slots, &tuning, cmd) {
                            if !dirty.contains(&rank) {
                                dirty.push(rank);
                            }
                        }
                    }
                    let now = Instant::now();
                    for (rank, slot) in slots.iter_mut().enumerate() {
                        if let Some((probe, _)) = slot.probe.filter(|(_, end)| now >= *end) {
                            slot.probe = None;
                            slot.state.settle_after_probe(probe); // so what the probe started is explicitly stopped
                            slot.dirty = true;
                            if !dirty.contains(&rank) {
                                dirty.push(rank);
                            }
                        }
                    }
                    for (rank, slot) in slots.iter_mut().enumerate() {
                        if let Some(motor) = slot.haptic_rumble.expire(now) {
                            set_haptic_motor(&mut slot.state, &mut slot.dirty, motor);
                            if !dirty.contains(&rank) {
                                dirty.push(rank);
                            }
                        }
                    }
                    for &rank in &dirty {
                        write(&mut api, &mut slots[rank], rank);
                    }
                    for (rank, slot) in slots.iter_mut().enumerate() {
                        if pump(slot, now) {
                            write(&mut api, slot, rank); // rumble gating changed: the state report must follow
                        }
                    }
                }
            })
            .expect("hid thread");
        Self { tx }
    }

    /// `rank`: which DualSense this is, counting connected DualSense pads in connect order from 0.
    pub fn apply(&self, rank: usize, out: &PadOutput) {
        let _ = self.tx.send(Cmd::Apply(rank, out.clone()));
    }

    pub fn haptic(&self, rank: usize, pcm: [u8; 120]) {
        let _ = self.tx.send(Cmd::Haptic(rank, pcm));
    }

    pub fn quiet(&self, rank: usize, quiet: bool) {
        let _ = self.tx.send(Cmd::Quiet(rank, quiet));
    }

    pub fn clear(&self, rank: usize) {
        let _ = self.tx.send(Cmd::Clear(rank));
    }

    pub fn close(&self, rank: usize) {
        let _ = self.tx.send(Cmd::Close(rank));
    }

    /// The user's rumble and trigger strength (see `Tuning`), for every pad.
    pub fn tune(&self, tuning: Tuning) {
        let _ = self.tx.send(Cmd::Tune(tuning));
    }

    /// A Test button: the pad rumbles or resists for a moment, then goes back to what the console asked for.
    pub fn probe(&self, rank: usize, probe: Probe) {
        let _ = self.tx.send(Cmd::Probe(rank, probe));
    }
}

/// The rumble made from haptics changed; the report must carry it even if the console never asked for rumble itself.
fn set_haptic_motor(state: &mut OutState, dirty: &mut bool, motor: u8) {
    state.haptic_motor = motor;
    state.rumble.get_or_insert((0, 0));
    *dirty = true;
}

/// Update the remembered state; the pad that needs a write, if any.
fn handle(slots: &mut Vec<Slot>, tuning: &Tuning, cmd: Cmd) -> Option<usize> {
    let rank = match &cmd {
        Cmd::Apply(r, _) | Cmd::Haptic(r, _) | Cmd::Quiet(r, _) | Cmd::Clear(r) | Cmd::Close(r) | Cmd::Probe(r, _) => {
            *r
        }
        Cmd::Tune(_) => return None, // handled by the caller: it concerns every pad
    };
    while slots.len() <= rank {
        slots.push(Slot::new(*tuning));
    }
    let s = &mut slots[rank];
    match cmd {
        Cmd::Apply(_, o) => {
            s.state.apply(&o);
            s.dirty = true;
        }
        Cmd::Haptic(_, pcm) => s.haptics.push_back(pcm),
        Cmd::Quiet(_, q) => {
            if s.quiet != q {
                s.quiet = q;
                s.dirty = true;
            }
            if q {
                s.haptic_rumble.stop();
                s.state.haptic_motor = 0;
                settle_coils(s);
            }
        }
        Cmd::Clear(_) => {
            s.state = s.state.silenced();
            s.quiet = false;
            s.dirty = true;
            s.haptics.clear();
            s.haptic_rumble.stop();
            settle_coils(s);
        }
        Cmd::Probe(_, probe) => {
            s.probe = Some((probe, Instant::now() + probe.duration()));
            s.dirty = true;
        }
        Cmd::Tune(_) => {}
        Cmd::Close(_) => {
            s.probe = None;
            settle_coils(s);
            s.dev = None;
            s.haptics.clear();
            s.haptic_rumble.stop();
            s.state.haptic_motor = 0;
            return None;
        }
    }
    Some(rank)
}

fn write(api: &mut HidApi, slot: &mut Slot, rank: usize) {
    if slot.dev.is_none() {
        let now = Instant::now();
        if slot.retry.blocked(now) {
            slot.haptics.clear();
            return;
        }
        if let Err(e) = api.refresh_devices() {
            slot.haptics.clear();
            let skipped = slot.retry.fail(now, RETRY_AFTER);
            return log::warn!("could not refresh the controller HID list: {e} ({skipped} attempts skipped)");
        }
        let mut found: Vec<_> = api
            .device_list()
            .filter(|d| d.vendor_id() == SONY && matches!(d.product_id(), DUALSENSE | DUALSENSE_EDGE))
            .collect();
        found.sort_by(|a, b| a.path().cmp(b.path()));
        let Some(info) = found.get(rank) else {
            slot.haptics.clear();
            let skipped = slot.retry.fail(now, RETRY_AFTER);
            if !slot.warned_missing_device {
                log::warn!(
                    "no DualSense HID device found for connected controller rank {rank} ({skipped} attempts skipped)"
                );
                slot.warned_missing_device = true;
            }
            return;
        };
        let bus = if matches!(info.bus_type(), BusType::Usb) { Bus::Usb } else { Bus::Bluetooth };
        match info.open_device(api) {
            Ok(d) => {
                let mut report = [0u8; 64];
                report[0] = 0x20;
                slot.state.firmware =
                    d.get_feature_report(&mut report).ok().and_then(|n| firmware_version(&report[..n]));
                log::info!("controller feedback on {bus:?}, firmware {:#06x?}", slot.state.firmware);
                slot.dev = Some((d, bus));
                slot.warned_missing_device = false;
                slot.retry.clear();
                slot.setup_ok = false;
                slot.stream = HapticStream::new(true);
                slot.next_tick = None;
                slot.dirty = true; // the firmware decides how rumble is written
            }
            Err(e) => {
                slot.haptics.clear();
                let skipped = slot.retry.fail(now, RETRY_AFTER);
                return log::warn!("cannot open the controller for feedback: {e} ({skipped} attempts skipped)");
            }
        }
    }
    let Some((dev, bus)) = &slot.dev else { return };
    if *bus == Bus::Usb && !slot.quiet {
        let now = Instant::now();
        let mut motor = None;
        while let Some(pcm) = slot.haptics.pop_front() {
            motor = slot.haptic_rumble.feed(&pcm, now).or(motor);
        }
        if let Some(motor) = motor {
            set_haptic_motor(&mut slot.state, &mut slot.dirty, motor);
        }
    }
    if slot.dirty {
        let mut state = if slot.quiet { slot.state.silenced() } else { slot.state.clone() };
        slot.gated = *bus == Bus::Bluetooth && !slot.quiet && slot.stream.delivering();
        if slot.gated {
            state = state.held_back_for_haptics();
        }
        if let Some((probe, _)) = slot.probe {
            state = slot.state.probing(probe); // a Test button wins over everything else for its moment
        }
        slot.seq = (slot.seq + 1) & 0x0F;
        if let Err(e) = dev.write(&build(&state, *bus, slot.seq)) {
            slot.dev = None;
            slot.haptics.clear();
            return log::warn!("could not write controller feedback: {e}");
        }
        slot.dirty = false;
    }
    if slot.quiet {
        slot.haptics.clear();
        return;
    }
    if *bus == Bus::Usb {
        return;
    }
    let intensity = slot.state.intensity.map_or(crate::report::DEFAULT_INTENSITY, |(haptic, _)| haptic);
    while let Some(pcm) = slot.haptics.pop_front() {
        slot.stream.offer(&pcm, intensity, slot.state.tuning.rumble_pct);
    }
    if slot.next_tick.is_none() && slot.stream.wants_tick() {
        slot.next_tick = Some(Instant::now());
    }
}

/// Bluetooth haptics: writes the blocks that are due. True when console rumble has to be re-written because haptics started or
/// stopped reaching the pad.
fn pump(slot: &mut Slot, now: Instant) -> bool {
    if slot.quiet || !matches!(slot.dev, Some((_, Bus::Bluetooth))) {
        return false;
    }
    let was_delivering = slot.stream.delivering();
    let mut due = 0;
    while slot.next_tick.is_some_and(|t| now >= t) && due < 3 {
        due += 1;
        slot.next_tick = slot.next_tick.map(|t| t + Duration::from_nanos(PERIOD_NS));
        let Some(out) = slot.stream.tick() else { continue };
        if !slot.setup_ok {
            let reduction = slot.state.intensity.map_or(0, |(_, trigger)| trigger_reduction(trigger));
            slot.seq = (slot.seq + 1) & 0x0F;
            if !write_report(slot, &setup_report(slot.seq, reduction), "the haptic coil setup") {
                return false;
            }
            slot.setup_ok = true;
            log::info!("Bluetooth haptics started");
        }
        let (report, blocks) = match &out {
            Out::Single(b) => (single_report(b, slot.seq, slot.counter), 1),
            Out::Pair(p) => (pair_report(p, slot.seq, slot.counter), 2),
        };
        slot.seq = (slot.seq + 1) & 0x0F;
        let ok = write_report(slot, &report, "Bluetooth haptics");
        slot.stream.accepted(ok);
        if !ok {
            return false;
        }
        slot.counter = slot.counter.wrapping_add(blocks);
    }
    // a stall (the thread was late by more than a couple of blocks): do not fire the missed ones in a burst
    if slot.next_tick.is_some_and(|t| now > t + Duration::from_nanos(2 * PERIOD_NS)) {
        slot.next_tick = Some(now);
    }
    if !slot.stream.wants_tick() {
        slot.next_tick = None;
    } else if slot.next_tick.is_none() {
        slot.next_tick = Some(now);
    }
    let changed = was_delivering != slot.stream.delivering();
    if changed {
        slot.dirty = true;
    }
    changed
}

/// Writes one report; a pad that refuses it is closed, so the next command finds it again.
fn write_report(slot: &mut Slot, report: &[u8], what: &str) -> bool {
    let Some((dev, _)) = &slot.dev else { return false };
    match dev.write(report) {
        Ok(_) => true,
        Err(e) => {
            slot.dev = None;
            slot.haptics.clear();
            log::warn!("could not write {what}: {e}");
            false
        }
    }
}

/// The haptics are over: one block of silence, so the coils do not hold the last sample.
fn settle_coils(slot: &mut Slot) {
    if !slot.stream.stop() || !matches!(slot.dev, Some((_, Bus::Bluetooth))) {
        slot.next_tick = None;
        return;
    }
    slot.next_tick = None;
    slot.seq = (slot.seq + 1) & 0x0F;
    let report = silence_report(slot.seq, slot.counter);
    slot.counter = slot.counter.wrapping_add(1);
    write_report(slot, &report, "the haptics silence");
}
