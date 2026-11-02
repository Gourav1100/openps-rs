//! Who drives the console pad. Controller mode is on while a physical pad is connected and keyboard mapping is off;
//! pressing Controls turns the mapping on and hands the pad to the keyboard and mouse, pressing it again hands it back.
//! Only the active side ever writes to the console, and a hand-over, a disconnect or a lost focus sends a neutral state.

use crate::{PadEvent, PadKind, Sensors, MAX_PADS};
use openps_input::state::ControllerState;
use openps_input::trace;

#[derive(Clone, Debug, PartialEq)]
pub enum PadCmd {
    /// A pad joined or left the console's controller list (slot 0 is always there for the keyboard pad).
    Connected {
        slot: usize,
        connected: bool,
    },
    State {
        slot: usize,
        state: ControllerState,
    },
    Sensors {
        slot: usize,
        sensors: Sensors,
    },
}

struct Slot {
    id: u32,
    kind: PadKind,
    name: String,
    state: ControllerState,
    sensors: Option<Sensors>,
    battery: Option<(f32, bool)>,
}

#[derive(Default)]
pub struct Arbiter {
    slots: [Option<Slot>; MAX_PADS],
    keyboard_mode: bool,
    focused: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PadInfo {
    pub slot: usize,
    pub kind: PadKind,
    pub name: String,
    pub battery: Option<(f32, bool)>,
}

impl Arbiter {
    /// True when the physical pad owns console slot 0, so the keyboard engine must stay silent.
    pub fn controller_active(&self) -> bool {
        self.slots[0].is_some() && self.live()
    }

    pub fn keyboard_mode(&self) -> bool {
        self.keyboard_mode
    }

    pub fn pads(&self) -> Vec<PadInfo> {
        (0..MAX_PADS)
            .filter_map(|i| {
                self.slots[i].as_ref().map(|s| PadInfo {
                    slot: i,
                    kind: s.kind,
                    name: s.name.clone(),
                    battery: s.battery,
                })
            })
            .collect()
    }

    pub fn slot_of(&self, id: u32) -> Option<usize> {
        self.slots.iter().position(|s| s.as_ref().is_some_and(|s| s.id == id))
    }

    /// True while the physical pads, not the key map, are the ones in play.
    pub fn is_live(&self) -> bool {
        self.live()
    }

    /// Console slot -> which DualSense it is (counting connected DualSense pads in connect order from 0). DS4 pads have no entry.
    pub fn dualsense_ranks(&self) -> Vec<(usize, usize)> {
        let mut v: Vec<(u32, usize)> = (0..MAX_PADS)
            .filter_map(|i| self.slots[i].as_ref().filter(|s| s.kind == PadKind::DualSense).map(|s| (s.id, i)))
            .collect();
        v.sort();
        v.into_iter().enumerate().map(|(rank, (_, slot))| (slot, rank)).collect()
    }

    fn live(&self) -> bool {
        !self.keyboard_mode && self.focused
    }

    pub fn on_event(&mut self, ev: PadEvent) -> Vec<PadCmd> {
        match ev {
            PadEvent::Connected { id, kind, name } => {
                if self.slot_of(id).is_some() {
                    return vec![];
                }
                let Some(slot) = self.slots.iter().position(Option::is_none) else { return vec![] };
                self.slots[slot] =
                    Some(Slot { id, kind, name, state: ControllerState::default(), sensors: None, battery: None });
                if slot == 0 {
                    vec![]
                } else {
                    vec![PadCmd::Connected { slot, connected: true }]
                }
            }

            PadEvent::State { id, state } => {
                let Some(slot) = self.slot_of(id) else { return vec![] };
                let state = state.clamped();
                let live = self.live();
                let s = self.slots[slot].as_mut().expect("slot_of found it");
                let previous = s.state.clone();
                if s.state == state {
                    if trace::enabled() {
                        log::info!(
                            "input_trace t={} stage=arbiter pad={id} slot={slot} forwarded=false reason=unchanged in[{}]",
                            trace::now_us(),
                            trace::brief(&state)
                        );
                    }
                    return vec![];
                }
                s.state = state.clone();
                if trace::enabled() {
                    log::info!(
                        "input_trace t={} stage=arbiter pad={id} slot={slot} forwarded={live} in[{}] mem_before[{}] mem_after[{}]",
                        trace::now_us(),
                        trace::brief(&state),
                        trace::brief(&previous),
                        trace::brief(&s.state)
                    );
                }
                if live {
                    vec![PadCmd::State { slot, state }]
                } else {
                    vec![]
                }
            }

            PadEvent::Sensors { id, sensors } => {
                let Some(slot) = self.slot_of(id) else { return vec![] };
                let s = self.slots[slot].as_mut().expect("slot_of found it");
                if s.sensors == Some(sensors) {
                    return vec![];
                }
                s.sensors = Some(sensors);
                if self.live() {
                    vec![PadCmd::Sensors { slot, sensors }]
                } else {
                    vec![]
                }
            }

            PadEvent::Battery { id, level, charging } => {
                if let Some(slot) = self.slot_of(id) {
                    self.slots[slot].as_mut().expect("slot_of found it").battery = Some((level, charging));
                }
                vec![]
            }
            PadEvent::Disconnected { id } => {
                let Some(slot) = self.slot_of(id) else { return vec![] };
                let had_sensors = self.slots[slot].as_ref().is_some_and(|s| s.sensors.is_some());
                self.slots[slot] = None;
                let mut out = vec![];
                if self.live() || slot != 0 {
                    out.push(PadCmd::State { slot, state: ControllerState::default() });
                    if had_sensors {
                        out.push(PadCmd::Sensors { slot, sensors: Sensors::idle() });
                    }
                }
                if slot != 0 {
                    out.push(PadCmd::Connected { slot, connected: false });
                }
                out
            }
        }
    }

    /// A new console session started: it knows nothing yet, so announce the extra pads and send every state again.
    pub fn resync(&self) -> Vec<PadCmd> {
        let mut out = vec![];
        for (slot, s) in self.slots.iter().enumerate() {
            let Some(s) = s else { continue };
            if slot != 0 {
                out.push(PadCmd::Connected { slot, connected: true });
            }
            if self.live() {
                out.push(PadCmd::State { slot, state: s.state.clone() });
                if let Some(sensors) = s.sensors {
                    out.push(PadCmd::Sensors { slot, sensors });
                }
            }
        }
        out
    }

    /// The Controls button: keyboard and mouse mapping on (`true`) or off (`false`).
    pub fn set_keyboard_mode(&mut self, on: bool) -> Vec<PadCmd> {
        if self.keyboard_mode == on {
            return vec![];
        }
        let was_live = self.live();
        self.keyboard_mode = on;
        self.handover(was_live)
    }

    pub fn set_focused(&mut self, focused: bool) -> Vec<PadCmd> {
        if self.focused == focused {
            return vec![];
        }
        let was_live = self.live();
        self.focused = focused;
        self.handover(was_live)
    }

    fn handover(&self, was_live: bool) -> Vec<PadCmd> {
        let live = self.live();
        if was_live == live {
            return vec![];
        }
        let mut out = vec![];
        for (slot, s) in self.slots.iter().enumerate() {
            let Some(s) = s else { continue };
            if live {
                out.push(PadCmd::State { slot, state: s.state.clone() });
                if let Some(sensors) = s.sensors {
                    out.push(PadCmd::Sensors { slot, sensors });
                }
            } else {
                out.push(PadCmd::State { slot, state: ControllerState::default() });
                if s.sensors.is_some() {
                    out.push(PadCmd::Sensors { slot, sensors: Sensors::idle() });
                }
            }
        }
        out
    }
}
