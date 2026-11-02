use openps_gamepad::mode::{Arbiter, PadCmd};
use openps_gamepad::{PadEvent, PadKind, Sensors};
use openps_input::state::{Button, ControllerState};

fn pressed() -> ControllerState {
    let mut s = ControllerState::default();
    s.buttons.insert(Button::CROSS);
    s
}
fn up(id: u32, state: ControllerState) -> PadEvent {
    PadEvent::State { id, state }
}
fn conn(id: u32) -> PadEvent {
    PadEvent::Connected { id, kind: PadKind::DualSense, name: format!("pad{id}") }
}
fn ready() -> Arbiter {
    let mut a = Arbiter::default();
    a.set_focused(true);
    a.on_event(conn(10));
    a
}

#[test]
fn a_connected_pad_drives_the_console_until_controls_is_pressed() {
    let mut a = ready();
    assert!(a.controller_active());
    assert_eq!(a.on_event(up(10, pressed())), vec![PadCmd::State { slot: 0, state: pressed() }]);
    let off = a.set_keyboard_mode(true);
    assert_eq!(off, vec![PadCmd::State { slot: 0, state: ControllerState::default() }]);
    assert!(!a.controller_active());
    assert!(a.on_event(up(10, ControllerState::default())).is_empty(), "keyboard mode: the pad stays silent");
    a.on_event(up(10, pressed()));
    assert_eq!(
        a.set_keyboard_mode(false),
        vec![PadCmd::State { slot: 0, state: pressed() }],
        "pressed again: the held state resumes"
    );
}

#[test]
fn without_a_pad_the_keyboard_is_never_gated() {
    let mut a = Arbiter::default();
    a.set_focused(true);
    assert!(!a.controller_active());
}

#[test]
fn identical_states_are_not_resent() {
    let mut a = ready();
    a.on_event(up(10, pressed()));
    assert!(a.on_event(up(10, pressed())).is_empty());
}

#[test]
fn losing_focus_releases_everything_and_regaining_it_restores() {
    let mut a = ready();
    a.on_event(up(10, pressed()));
    assert_eq!(a.set_focused(false), vec![PadCmd::State { slot: 0, state: ControllerState::default() }]);
    assert!(!a.controller_active());
    assert!(a.on_event(up(10, pressed())).is_empty());
    assert_eq!(a.set_focused(true), vec![PadCmd::State { slot: 0, state: pressed() }]);
}

#[test]
fn disconnect_releases_the_slot_and_hands_the_console_back() {
    let mut a = ready();
    a.on_event(up(10, pressed()));
    assert_eq!(
        a.on_event(PadEvent::Disconnected { id: 10 }),
        vec![PadCmd::State { slot: 0, state: ControllerState::default() }]
    );
    assert!(!a.controller_active());
}

#[test]
fn extra_pads_take_the_next_slots_and_are_announced_to_the_console() {
    let mut a = ready();
    assert_eq!(a.on_event(conn(11)), vec![PadCmd::Connected { slot: 1, connected: true }]);
    assert_eq!(a.on_event(conn(12)), vec![PadCmd::Connected { slot: 2, connected: true }]);
    a.on_event(conn(13));
    assert!(a.on_event(conn(14)).is_empty(), "only four pads");
    let gone = a.on_event(PadEvent::Disconnected { id: 11 });
    assert_eq!(gone.last(), Some(&PadCmd::Connected { slot: 1, connected: false }));
    assert_eq!(a.on_event(conn(15)), vec![PadCmd::Connected { slot: 1, connected: true }], "the freed slot is reused");
}

#[test]
fn sensors_are_sent_only_when_they_change_and_only_when_live() {
    let mut a = ready();
    let s = Sensors { gyro: [1.0, 0.0, 0.0], ..Default::default() };
    let ev = PadEvent::Sensors { id: 10, sensors: s };
    assert_eq!(a.on_event(ev.clone()), vec![PadCmd::Sensors { slot: 0, sensors: s }]);
    assert!(a.on_event(ev).is_empty());
}

#[test]
fn out_of_range_values_are_clamped_before_they_are_sent() {
    let mut a = ready();
    let wild = ControllerState { left_x: 9.0, l2: f64::NAN, ..Default::default() };
    let out = a.on_event(up(10, wild));
    let PadCmd::State { state, .. } = &out[0] else { panic!() };
    assert_eq!((state.left_x, state.l2), (1.0, 0.0));
}

#[test]
fn unknown_pads_are_ignored_and_battery_is_listed() {
    let mut a = ready();
    assert!(a.on_event(up(99, pressed())).is_empty());
    a.on_event(PadEvent::Battery { id: 10, level: 0.5, charging: true });
    let p = &a.pads()[0];
    assert_eq!((p.slot, p.battery), (0, Some((0.5, true))));
}

fn moving() -> Sensors {
    Sensors { gyro: [1.0, 2.0, 3.0], accel: [0.1, 0.9, 0.2], orientation: [0.5, 0.5, 0.5, 0.5] }
}

#[test]
fn handing_the_console_over_also_resets_the_motion_sensors() {
    // chiaki-ng resets gyro/accel/orientation to idle when a controller goes away; otherwise the console keeps seeing the last
    // non-zero motion (the stream resends it every 100 ms) and motion-aim drifts.
    let mut a = ready();
    a.on_event(PadEvent::Sensors { id: 10, sensors: moving() });
    let off = a.set_keyboard_mode(true);
    assert_eq!(
        off,
        vec![
            PadCmd::State { slot: 0, state: ControllerState::default() },
            PadCmd::Sensors { slot: 0, sensors: Sensors::idle() },
        ]
    );
    let back = a.set_keyboard_mode(false);
    assert!(
        back.contains(&PadCmd::Sensors { slot: 0, sensors: moving() }),
        "the held motion resumes with the pad: {back:?}"
    );
}

#[test]
fn losing_focus_and_unplugging_reset_the_motion_sensors_too() {
    let mut a = ready();
    a.on_event(PadEvent::Sensors { id: 10, sensors: moving() });
    assert!(a.set_focused(false).contains(&PadCmd::Sensors { slot: 0, sensors: Sensors::idle() }));
    a.set_focused(true);
    let gone = a.on_event(PadEvent::Disconnected { id: 10 });
    assert!(gone.contains(&PadCmd::Sensors { slot: 0, sensors: Sensors::idle() }), "{gone:?}");
}

#[test]
fn a_pad_that_never_reported_motion_gets_no_sensor_command() {
    let mut a = ready();
    let off = a.set_keyboard_mode(true);
    assert!(off.iter().all(|c| !matches!(c, PadCmd::Sensors { .. })), "{off:?}");
}

#[test]
fn the_idle_sensor_values_are_level_and_at_rest() {
    let s = Sensors::idle();
    assert_eq!((s.gyro, s.accel, s.orientation), ([0.0; 3], [0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]));
}

#[test]
fn a_button_let_go_in_the_background_is_corrected_by_the_fresh_reading_on_refocus() {
    // GameController delivers nothing while the app is in the background, so the arbiter still remembers the button as held and
    // replays it on refocus. The pad is read again right then (MacPads::resnapshot), and that reading must undo the stale press.
    let mut a = ready();
    a.on_event(up(10, pressed()));
    a.set_focused(false);
    let back = a.set_focused(true);
    assert_eq!(back, vec![PadCmd::State { slot: 0, state: pressed() }], "the stale press is replayed");
    let fresh = a.on_event(up(10, ControllerState::default()));
    assert_eq!(
        fresh,
        vec![PadCmd::State { slot: 0, state: ControllerState::default() }],
        "the fresh reading releases it"
    );
}

#[test]
fn a_fresh_reading_that_matches_the_memory_sends_nothing() {
    let mut a = ready();
    a.on_event(up(10, pressed()));
    a.set_focused(false);
    a.set_focused(true);
    assert!(a.on_event(up(10, pressed())).is_empty(), "still held: no duplicate");
}
