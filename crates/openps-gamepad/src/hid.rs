//! Writes output reports to DualSense pads on a thread of its own. The pad is opened shared, next to GameController (which keeps
//! reading the input). Writes happen on change only; the newest request per pad wins.

use crate::haptics::bluetooth_report;
use crate::report::{build, Bus, OutState};
use hidapi::{BusType, HidApi, HidDevice};
use openps_stream::padblob::PadOutput;
use std::collections::VecDeque;
use std::sync::mpsc::{channel, Sender};

const SONY: u16 = 0x054C;
const DUALSENSE: u16 = 0x0CE6;
const DUALSENSE_EDGE: u16 = 0x0DF2;

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
    warned_usb_haptics: bool,
    warned_missing_device: bool,
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
                while let Ok(first) = rx.recv() {
                    let mut dirty = vec![];
                    for cmd in std::iter::once(first).chain(std::iter::from_fn(|| rx.try_recv().ok())) {
                        if let Some(rank) = handle(&mut slots, cmd) {
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
            warned_usb_haptics: false,
            warned_missing_device: false,
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
        }
        Cmd::Clear(_) => {
            s.state = s.state.silenced();
            s.quiet = false;
            s.dirty = true;
            s.haptics.clear();
        }
        Cmd::Close(_) => {
            s.dev = None;
            s.haptics.clear();
            return None;
        }
    }
    Some(rank)
}

fn write(api: &mut HidApi, slot: &mut Slot, rank: usize) {
    if slot.dev.is_none() {
        if let Err(e) = api.refresh_devices() {
            slot.haptics.clear();
            return log::warn!("could not refresh the controller HID list: {e}");
        }
        let mut found: Vec<_> = api
            .device_list()
            .filter(|d| d.vendor_id() == SONY && matches!(d.product_id(), DUALSENSE | DUALSENSE_EDGE))
            .collect();
        found.sort_by(|a, b| a.path().cmp(b.path()));
        let Some(info) = found.get(rank) else {
            slot.haptics.clear();
            if !slot.warned_missing_device {
                log::warn!("no DualSense HID device found for connected controller rank {rank}");
                slot.warned_missing_device = true;
            }
            return;
        };
        let bus = if matches!(info.bus_type(), BusType::Usb) { Bus::Usb } else { Bus::Bluetooth };
        match info.open_device(api) {
            Ok(d) => {
                slot.dev = Some((d, bus));
                slot.warned_usb_haptics = false;
                slot.warned_missing_device = false;
            }
            Err(e) => {
                slot.haptics.clear();
                return log::warn!("cannot open the controller for feedback: {e}");
            }
        }
    }
    let Some((dev, bus)) = &slot.dev else { return };
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
        if !slot.haptics.is_empty() && !slot.warned_usb_haptics {
            log::warn!("voice-coil PCM haptics over USB need the controller's CoreAudio output, not HID");
            slot.warned_usb_haptics = true;
        }
        slot.haptics.clear();
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
