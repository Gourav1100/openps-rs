//! Writes output reports to DualSense pads on a thread of its own. The pad is opened shared, next to GameController (which keeps
//! reading the input). Writes happen on change only; the newest request per pad wins.

use crate::backoff::Backoff;
use crate::haptic_rumble::HapticRumble;
use crate::haptics::bluetooth_report;
use crate::report::{build, Bus, OutState};
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
}

#[derive(Clone)]
pub struct HidOut {
    tx: Sender<Cmd>,
}

struct Slot {
    state: OutState,
    quiet: bool,
    dev: Option<(HidDevice, Bus)>,
    seq: u8,
    haptic_seq: u8,
    haptic_counter: u8,
    dirty: bool,
    haptics: VecDeque<[u8; 120]>,
    /// USB only: the console's haptic audio played as rumble, since the voice coils need the pad's USB audio device.
    haptic_rumble: HapticRumble,
    warned_missing_device: bool,
    retry: Backoff,
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
                loop {
                    // Wake for the earliest rumble-from-haptics deadline so the motors always stop when the audio does.
                    let wake = slots.iter().filter_map(|s| s.haptic_rumble.deadline()).min();
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
                        if let Some(rank) = handle(&mut slots, cmd) {
                            if !dirty.contains(&rank) {
                                dirty.push(rank);
                            }
                        }
                    }
                    let now = Instant::now();
                    for (rank, slot) in slots.iter_mut().enumerate() {
                        if let Some(motor) = slot.haptic_rumble.expire(now) {
                            set_haptic_motor(&mut slot.state, &mut slot.dirty, motor);
                            if !dirty.contains(&rank) {
                                dirty.push(rank);
                            }
                        }
                    }
                    for rank in dirty {
                        write(&mut api, &mut slots[rank], rank);
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
}

/// The rumble made from haptics changed; the report must carry it even if the console never asked for rumble itself.
fn set_haptic_motor(state: &mut OutState, dirty: &mut bool, motor: u8) {
    state.haptic_motor = motor;
    state.rumble.get_or_insert((0, 0));
    *dirty = true;
}

/// Update the remembered state; the pad that needs a write, if any.
fn handle(slots: &mut Vec<Slot>, cmd: Cmd) -> Option<usize> {
    let rank = match &cmd {
        Cmd::Apply(r, _) | Cmd::Haptic(r, _) | Cmd::Quiet(r, _) | Cmd::Clear(r) | Cmd::Close(r) => *r,
    };
    while slots.len() <= rank {
        slots.push(Slot {
            state: OutState::default(),
            quiet: false,
            dev: None,
            seq: 0,
            haptic_seq: 0,
            haptic_counter: 0xFF,
            dirty: false,
            haptics: VecDeque::new(),
            haptic_rumble: HapticRumble::default(),
            warned_missing_device: false,
            retry: Backoff::default(),
        });
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
            }
        }
        Cmd::Clear(_) => {
            s.state = s.state.silenced();
            s.quiet = false;
            s.dirty = true;
            s.haptics.clear();
            s.haptic_rumble.stop();
        }
        Cmd::Close(_) => {
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
                slot.dev = Some((d, bus));
                slot.warned_missing_device = false;
                slot.retry.clear();
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
        let state = if slot.quiet { slot.state.silenced() } else { slot.state.clone() };
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
    while let Some(pcm) = slot.haptics.pop_front() {
        slot.haptic_counter = slot.haptic_counter.wrapping_add(2);
        let intensity = slot.state.intensity.map_or(crate::report::DEFAULT_INTENSITY, |(haptic, _)| haptic);
        let report = bluetooth_report(&pcm, slot.haptic_seq, slot.haptic_counter, intensity);
        slot.haptic_seq = (slot.haptic_seq + 1) & 0x0F;
        if let Err(e) = dev.write(&report) {
            slot.dev = None;
            slot.haptics.clear();
            return log::warn!("could not write controller PCM haptics: {e}");
        }
    }
}
