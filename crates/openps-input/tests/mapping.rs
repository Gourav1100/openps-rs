//! Ported from Con2PC tests/test_mapping.py and tests/test_mapper_features.py.
use openps_input::keys::{parse_chord, parse_input, Input, Key, MouseInput};
use openps_input::state::{Button, ControllerState};
use openps_input::{load_defaults, load_layers, Config, InputSnapshot, Mapper};

const L: Input = Input::Mouse(MouseInput::MOUSE_LEFT);
const R: Input = Input::Mouse(MouseInput::MOUSE_RIGHT);
const CMD: Input = Input::Key(Key::LEFT_COMMAND);
const TAB: Input = Input::Key(Key::TAB);

fn k(key: Key) -> Input {
    Input::Key(key)
}
fn snap(inputs: &[Input]) -> InputSnapshot {
    InputSnapshot::new(inputs.iter().copied(), 0.0, 0.0)
}
fn snapd(inputs: &[Input], dx: f64, dy: f64) -> InputSnapshot {
    InputSnapshot::new(inputs.iter().copied(), dx, dy)
}
fn approx(a: f64, b: f64) {
    assert!((a - b).abs() < 1e-6, "{a} != {b}");
}
fn approx_tol(a: f64, b: f64, tol: f64) {
    assert!((a - b).abs() <= tol, "{a} != {b} (tol {tol})");
}
fn cfg() -> Config {
    load_defaults().unwrap()
}
fn mapper() -> Mapper {
    Mapper::new(cfg(), None).unwrap()
}
fn mapper_with(f: impl FnOnce(&mut Config)) -> Mapper {
    let mut c = cfg();
    f(&mut c);
    Mapper::new(c, None).unwrap()
}
fn user(text: &str) -> Config {
    load_layers(Some(text), false).unwrap()
}
fn run(m: &mut Mapper, frames: &[InputSnapshot], step: f64, start: f64) -> Vec<ControllerState> {
    frames.iter().enumerate().map(|(i, f)| m.map(f, start + i as f64 * step)).collect()
}
fn rep(s: InputSnapshot, n: usize) -> Vec<InputSnapshot> {
    vec![s; n]
}

// ---- test_mapping.py ---------------------------------------------------------------------
#[test]
fn wasd_and_buttons() {
    let st = mapper().map(&snap(&[k(Key::W), k(Key::D), k(Key::SPACE)]), 0.0);
    approx(st.left_y, -(0.5f64.sqrt()));
    approx(st.left_x, 0.5f64.sqrt());
    assert!(st.buttons.contains(&Button::CROSS));
}

#[test]
fn opposing_keys_cancel() {
    assert_eq!(mapper().map(&snap(&[k(Key::A), k(Key::D)]), 0.0).left_x, 0.0);
}

#[test]
fn mouse_triggers_and_look_decay() {
    let mut m = mapper();
    let st = m.map(&snapd(&[L], 20.0, 0.0), 0.0);
    assert!(st.r2 == 1.0 && st.right_x > 0.0);
    let mut st = st;
    for i in 1..=60 {
        st = m.map(&snap(&[]), i as f64 * 0.004);
    }
    assert_eq!(st.right_x, 0.0);
}

#[test]
fn parse_names() {
    assert_eq!(parse_input("w").unwrap(), k(Key::W));
    assert_eq!(parse_input("1").unwrap(), k(Key::KEY_1));
    assert_eq!(parse_input("mouse_left").unwrap(), L);
    assert_eq!(parse_chord("left_shift+w").unwrap(), [k(Key::LEFT_SHIFT), k(Key::W)].into_iter().collect());
    assert!(parse_input("nonsense").is_err());
}

#[test]
fn user_config_chords_and_combos() {
    let mut c = user("toggle = \"LEFT_CONTROL+F5\"\n[bindings]\nTRIANGLE = [\"LEFT_SHIFT+Q\"]\nCROSS = [\"Z\"]\n[[combos]]\ninput = \"LEFT_OPTION+E\"\noutput = [\"L1\", \"CIRCLE\"]\n");
    c.output.min_press_ms = 0.0;
    assert_eq!(c.toggle_chord().unwrap(), [k(Key::LEFT_CONTROL), k(Key::F5)].into_iter().collect());
    let mut m = Mapper::new(c, None).unwrap();
    assert!(m.map(&snap(&[k(Key::Z)]), 0.0).buttons.contains(&Button::CROSS));
    assert!(!m.map(&snap(&[k(Key::SPACE)]), 0.0).buttons.contains(&Button::CROSS)); // default replaced
    let st = m.map(&snap(&[k(Key::LEFT_OPTION), k(Key::E)]), 0.0);
    assert!(st.buttons.contains(&Button::L1) && st.buttons.contains(&Button::CIRCLE));
    let st = m.map(&snap(&[k(Key::LEFT_SHIFT), k(Key::Q)]), 0.0); // longest chord wins
    assert!(st.buttons.contains(&Button::TRIANGLE) && !st.buttons.contains(&Button::SQUARE));
    assert!(m.map(&snap(&[k(Key::LEFT_SHIFT)]), 0.0).buttons.contains(&Button::L3));
}

#[test]
fn mouse_move_as_button() {
    let mut c = user("[mouse]\nlook = \"NONE\"\n[bindings]\nR1 = [\"MOUSE_MOVE_RIGHT\"]\n");
    c.output.min_press_ms = 0.0;
    let mut m = Mapper::new(c, None).unwrap();
    assert!(m.map(&snapd(&[], 10.0, 0.0), 0.0).buttons.contains(&Button::R1));
    assert!(!m.map(&snapd(&[], 1.0, 0.0), 0.0).buttons.contains(&Button::R1));
}

#[test]
fn bad_config_rejected() {
    assert!(load_layers(Some("[bindings]\nNOPE = [\"W\"]\n"), false).is_err());
}

#[test]
fn every_key_roundtrips() {
    for key in Key::ALL {
        assert_eq!(Key::from_vk(key.vk()), Some(*key));
    }
}

#[test]
fn mouse_recenters_when_idle() {
    let mut m = mapper();
    let mut t = 0.0;
    let mut st = ControllerState::default();
    for _ in 0..20 {
        t += 0.004;
        st = m.map(&snapd(&[], 2.0, 0.0), t);
    }
    assert!(st.right_x > 0.3 && st.right_x < 1.0);
    t += 0.004;
    assert!(m.map(&snap(&[]), t).right_x.abs() < st.right_x);
    t += 0.05;
    let st = m.map(&snap(&[]), t);
    assert!(st.right_x == 0.0 && st.right_y == 0.0);
}

#[test]
fn arrow_keys_drive_camera_with_ramp_and_recenter() {
    let mut m = mapper();
    let a = [k(Key::RIGHT_ARROW)];
    assert_eq!(m.map(&snap(&a), 0.0).right_x, 0.0);
    let x = m.map(&snap(&a), 0.05).right_x;
    assert!(x > 0.3 && x < 0.7);
    assert_eq!(m.map(&snap(&a), 0.2).right_x, 1.0);
    let x = m.map(&snap(&[]), 0.22).right_x;
    assert!(x > 0.0 && x < 1.0);
    assert_eq!(m.map(&snap(&[]), 0.3).right_x, 0.0);
}

#[test]
fn secondary_key_and_left_stick_instant() {
    let mut m = mapper();
    assert_eq!(m.map(&snap(&[k(Key::J)]), 0.0).right_x, 0.0);
    assert_eq!(m.map(&snap(&[k(Key::W)]), 0.01).left_y, -1.0);
}

#[test]
fn dpad_needs_option_chord() {
    let mut m = mapper();
    let c = [k(Key::LEFT_OPTION), k(Key::UP_ARROW)];
    assert!(m.map(&snap(&c), 0.0).buttons.contains(&Button::DPAD_UP));
    assert_eq!(m.map(&snap(&c), 0.5).right_y, 0.0); // chord wins over the plain arrow
}

#[test]
fn touchpad_swipe_stroke_once_per_press() {
    let mut m = mapper();
    let seen: Vec<_> = (0..60).map(|i| m.map(&snap(&[k(Key::KEY_4)]), i as f64 * 0.004).touch).collect();
    let xs: Vec<f64> = seen.iter().flatten().map(|t| t.0).collect();
    assert!(!xs.is_empty() && xs.windows(2).all(|w| w[0] <= w[1]));
    assert!(xs[0] < 0.5 && 0.5 < *xs.last().unwrap());
    assert!(seen.last().unwrap().is_none()); // finger lifted though the key is still held
    assert_eq!(seen.windows(2).filter(|w| w[0].is_none() && w[1].is_some()).count(), 0);
}

#[test]
fn touchpad_click_and_tap_bindings() {
    let mut m = mapper();
    assert!(m.map(&snap(&[k(Key::M)]), 0.0).buttons.contains(&Button::TOUCHPAD));
    assert!(m.map(&snap(&[k(Key::T)]), 1.0).buttons.contains(&Button::MUTE));
    assert_eq!(m.map(&snap(&[k(Key::KEY_7)]), 0.0).touch, Some((0.5, 0.5)));
}

#[test]
fn racing_profile_layout() {
    let c = cfg();
    assert_eq!(c.profile_names(), vec!["default", "racing", "assassins_creed"]);
    let mut m = Mapper::new(c, Some("racing")).unwrap();
    let st = m.map(&snap(&[k(Key::UP_ARROW), k(Key::RIGHT_ARROW)]), 0.0);
    assert!(st.r2 == 1.0 && st.left_x == 1.0 && st.right_y == 0.0);
    assert_eq!(m.map(&snap(&[k(Key::DOWN_ARROW)]), 0.1).l2, 1.0);
    assert_eq!(m.map(&snap(&[k(Key::S)]), 0.2).left_y, 0.0);
    // W/A/S/D is the D-pad here, and Option+arrows move the camera
    let st = m.map(&snap(&[k(Key::D)]), 0.25);
    assert!(st.buttons.contains(&Button::DPAD_RIGHT) && st.right_x == 0.0);
    let st = m.map(&snap(&[k(Key::LEFT_OPTION), k(Key::RIGHT_ARROW)]), 0.3);
    assert!(st.right_x > 0.0 && st.right_x < 1.0);
    assert_eq!(m.map(&snap(&[k(Key::LEFT_OPTION), k(Key::RIGHT_ARROW)]), 0.6).right_x, 1.0);
}

#[test]
fn switching_profiles_changes_layout_and_resets_state() {
    let mut m = mapper();
    assert_eq!(m.map(&snap(&[k(Key::W)]), 0.0).left_y, -1.0);
    assert_eq!(m.map(&snap(&[k(Key::UP_ARROW)]), 0.2).r2, 0.0);
    m.set_profile("racing").unwrap();
    assert_eq!(m.map(&snap(&[k(Key::W)]), 0.3).left_y, 0.0);
    assert_eq!(m.map(&snap(&[k(Key::UP_ARROW)]), 0.4).r2, 1.0);
    m.set_profile("default").unwrap();
    assert_eq!(m.map(&snap(&[k(Key::W)]), 0.5).left_y, -1.0);
}

#[test]
fn profile_merge_and_validation() {
    let c = user("[profiles.racing.bindings]\nCROSS = [\"Z\"]\n[profiles.drift.bindings]\nL1 = [\"Q\"]\n");
    assert_eq!(c.profile_bindings("racing")["CROSS"], vec!["Z"]);
    assert_eq!(c.profile_bindings("racing")["R2"][0], "UP_ARROW"); // shipped override survives
    assert_eq!(c.profile_names(), vec!["default", "racing", "assassins_creed", "drift"]);
    assert!(load_layers(Some("[profiles.default.bindings]\nCROSS = [\"Z\"]\n"), false).is_err());
}

// ---- test_mapper_features.py -------------------------------------------------------------
#[test]
fn w_then_d_keeps_w_and_goes_diagonal() {
    let mut m = mapper();
    let mut frames = rep(snap(&[k(Key::W)]), 5);
    frames.extend(rep(snap(&[k(Key::W), k(Key::D)]), 5));
    frames.extend(rep(snap(&[k(Key::W)]), 5));
    let s = run(&mut m, &frames, 0.004, 0.0);
    assert_eq!((s[4].left_x, s[4].left_y), (0.0, -1.0));
    approx(s[9].left_y, -(0.5f64.sqrt()));
    approx(s[9].left_x, 0.5f64.sqrt());
    assert_eq!((s[14].left_x, s[14].left_y), (0.0, -1.0));
}

#[test]
fn diagonal_is_exactly_45_degrees_at_full_speed() {
    let st = mapper().map(&snap(&[k(Key::W), k(Key::D)]), 0.0);
    approx(st.left_x.hypot(st.left_y), 1.0);
    approx((-st.left_y).atan2(st.left_x).to_degrees(), 45.0);
}

#[test]
fn any_combination_of_wasd_stays_inside_the_unit_circle() {
    let keys = [Key::W, Key::A, Key::S, Key::D];
    for mask in 1u32..16 {
        let held: Vec<Input> =
            keys.iter().enumerate().filter(|(i, _)| mask >> i & 1 == 1).map(|(_, kk)| k(*kk)).collect();
        let st = mapper().map(&snap(&held), 0.0);
        assert!(st.left_x.hypot(st.left_y) <= 1.0 + 1e-9);
    }
}

#[test]
fn opposite_keys_cancel_but_perpendicular_survives() {
    let st = mapper().map(&snap(&[k(Key::W), k(Key::A), k(Key::D)]), 0.0);
    assert!(st.left_x == 0.0 && st.left_y == -1.0);
}

#[test]
fn keys_buttons_and_mouse_all_work_at_the_same_time() {
    let mut m = mapper();
    let f = rep(snapd(&[k(Key::W), k(Key::D), k(Key::SPACE), k(Key::E), L], 6.0, -3.0), 8);
    let st = run(&mut m, &f, 0.004, 0.0).pop().unwrap();
    assert!(st.left_y < 0.0 && 0.0 < st.left_x);
    assert!(st.buttons.contains(&Button::CROSS) && st.buttons.contains(&Button::CIRCLE));
    assert!(st.r2 == 1.0 && st.right_x > 0.0 && st.right_y < 0.0);
}

#[test]
fn camera_keys_plus_mouse_never_exceed_the_unit_circle() {
    let mut m = mapper();
    let f = rep(snapd(&[k(Key::RIGHT_ARROW), k(Key::UP_ARROW)], 80.0, -80.0), 40);
    for st in run(&mut m, &f, 0.004, 0.0) {
        assert!(st.right_x.hypot(st.right_y) <= 1.0 + 1e-9);
    }
}

#[test]
fn a_quick_tap_still_lasts_the_minimum_press() {
    let mut m = mapper_with(|c| c.output.min_press_ms = 35.0);
    let mut pressed = vec![m.map(&snap(&[k(Key::SPACE)]), 0.0)];
    for t in [0.005, 0.020, 0.034] {
        pressed.push(m.map(&snap(&[]), t));
    }
    assert!(pressed.iter().all(|s| s.buttons.contains(&Button::CROSS)));
    assert!(!m.map(&snap(&[]), 0.040).buttons.contains(&Button::CROSS));
}

#[test]
fn min_press_does_not_extend_a_long_press() {
    let mut m = mapper_with(|c| c.output.min_press_ms = 35.0);
    run(&mut m, &rep(snap(&[k(Key::SPACE)]), 50), 0.004, 0.0);
    assert!(!m.map(&snap(&[]), 0.4).buttons.contains(&Button::CROSS));
}

#[test]
fn min_press_can_be_turned_off() {
    let mut m = mapper_with(|c| c.output.min_press_ms = 0.0);
    m.map(&snap(&[k(Key::SPACE)]), 0.0);
    assert!(!m.map(&snap(&[]), 0.001).buttons.contains(&Button::CROSS));
}

#[test]
fn trigger_is_full_while_held_and_resets_on_release() {
    let mut m = mapper();
    let mut f = rep(snap(&[L]), 30);
    f.extend(rep(snap(&[]), 30));
    let s = run(&mut m, &f, 0.004, 0.0);
    assert!(s[..30].iter().all(|x| x.r2 == 1.0));
    assert_eq!(s[59].r2, 0.0);
    assert_eq!(s[45].r2, 0.0);
}

#[test]
fn normal_click_registers_as_press_then_leave() {
    let mut m = mapper();
    let mut v = vec![m.map(&snap(&[R]), 0.0).l2];
    for t in [0.003, 0.01, 0.03, 0.06, 0.1] {
        v.push(m.map(&snap(&[]), t).l2);
    }
    assert_eq!(v, vec![1.0, 1.0, 1.0, 1.0, 0.0, 0.0]);
}

#[test]
fn left_trigger_follows_right_click_independently() {
    let st = mapper().map(&snap(&[R]), 0.0);
    assert!(st.l2 == 1.0 && st.r2 == 0.0);
}

fn adaptive(m: &mut Mapper, phases: &[(InputSnapshot, usize)], step: f64) -> Vec<ControllerState> {
    let mut out = vec![];
    let mut t = 0.0;
    for (frame, n) in phases {
        for _ in 0..*n {
            out.push(m.map(frame, t));
            t += step;
        }
    }
    out
}

#[test]
fn command_click_sweeps_up_then_down_while_held() {
    let mut m = mapper();
    let s = adaptive(&mut m, &[(snap(&[CMD, L]), 260)], 0.01);
    let l2: Vec<f64> = s.iter().map(|x| x.l2).collect();
    assert_eq!(l2[0], 0.0);
    approx_tol(l2[60], 0.5, 0.03);
    let max = l2.iter().cloned().fold(0.0, f64::max);
    approx_tol(max, 1.0, 0.01);
    let top = l2.iter().position(|v| *v == max).unwrap();
    approx_tol(l2[top + 60], 0.5, 0.05);
    assert!(*l2.last().unwrap() < 1.0);
}

#[test]
fn release_the_click_locks_the_value_until_command_is_released() {
    let mut m = mapper();
    let s = adaptive(&mut m, &[(snap(&[CMD, L]), 40), (snap(&[CMD]), 100)], 0.01);
    let locked = s[39].l2;
    assert!(locked > 0.2 && locked < 0.5);
    assert!(s[41..].iter().all(|x| (x.l2 - locked).abs() < 1e-6));
}

#[test]
fn releasing_command_resets_the_trigger() {
    let mut m = mapper();
    let s = adaptive(&mut m, &[(snap(&[CMD, L]), 40), (snap(&[CMD]), 20), (snap(&[]), 5)], 0.01);
    assert!(s[59].l2 > 0.2);
    assert_eq!(s.last().unwrap().l2, 0.0);
}

#[test]
fn a_second_click_continues_from_the_locked_value() {
    let mut m = mapper();
    let s = adaptive(&mut m, &[(snap(&[CMD, L]), 30), (snap(&[CMD]), 10), (snap(&[CMD, L]), 30)], 0.01);
    let locked = s[39].l2;
    assert!(s[41].l2 >= locked - 1e-9 && s[69].l2 > locked);
}

#[test]
fn command_right_click_drives_the_right_trigger_the_same_way() {
    let mut m = mapper();
    let s = adaptive(&mut m, &[(snap(&[CMD, R]), 40), (snap(&[CMD]), 10)], 0.01);
    let last = s.last().unwrap();
    assert!(last.r2 > 0.2 && last.r2 < 0.5 && last.l2 == 0.0);
}

#[test]
fn command_released_while_click_held_ignores_the_click_until_released() {
    let mut m = mapper();
    let s = adaptive(&mut m, &[(snap(&[CMD, L]), 30), (snap(&[L]), 10), (snap(&[]), 10), (snap(&[L]), 10)], 0.01);
    assert!(s[39].l2 == 0.0 && s[39].r2 == 0.0);
    let last = s.last().unwrap();
    assert!(last.r2 == 1.0 && last.l2 == 0.0);
}

#[test]
fn right_command_works_too_and_adaptive_can_be_disabled() {
    let mut m = mapper();
    let s = adaptive(&mut m, &[(snap(&[k(Key::RIGHT_COMMAND), L]), 40)], 0.01);
    assert!(s.last().unwrap().l2 > 0.2 && s.last().unwrap().l2 < 0.5);
    let mut m = mapper_with(|c| c.adaptive_triggers.enabled = false);
    let st = m.map(&snap(&[CMD, L]), 0.0);
    assert!(st.r2 == 1.0 && st.l2 == 0.0);
}

#[test]
fn sweep_speed_is_configurable() {
    let mut m = mapper_with(|c| c.adaptive_triggers.sweep_ms = 300.0);
    let s = adaptive(&mut m, &[(snap(&[CMD, L]), 30)], 0.01);
    approx_tol(s[29].l2, 0.9, 0.1);
}

fn toggle_click(m: &mut Mapper, button: Input, t: f64) -> ControllerState {
    m.map(&snap(&[TAB]), t);
    let st = m.map(&snap(&[TAB, button]), t + 0.01);
    m.map(&snap(&[TAB]), t + 0.06);
    m.map(&snap(&[]), t + 0.12);
    st
}

#[test]
fn tab_left_click_toggles_rt_to_rb_and_back() {
    let mut m = mapper();
    assert_eq!(m.click_modes(), ("RT", "LT"));
    let st = toggle_click(&mut m, L, 0.0);
    assert!(st.r2 == 0.0 && !st.buttons.contains(&Button::R1));
    assert_eq!(m.click_modes().0, "RB");
    let plain = m.map(&snap(&[L]), 1.0);
    assert!(plain.buttons.contains(&Button::R1) && plain.r2 == 0.0);
    m.map(&snap(&[]), 1.1);
    toggle_click(&mut m, L, 2.0);
    assert_eq!(m.click_modes().0, "RT");
    assert_eq!(m.map(&snap(&[L]), 3.0).r2, 1.0);
}

#[test]
fn tab_right_click_toggles_lt_to_lb_separately() {
    let mut m = mapper();
    toggle_click(&mut m, R, 0.0);
    assert_eq!(m.click_modes(), ("RT", "LB"));
    let st = m.map(&snap(&[R]), 1.0);
    assert!(st.buttons.contains(&Button::L1) && st.l2 == 0.0);
    assert_eq!(m.map(&snap(&[L]), 2.0).r2, 1.0);
}

#[test]
fn toggle_only_happens_on_the_click_edge_with_tab_already_down() {
    let mut m = mapper();
    m.map(&snap(&[L]), 0.0);
    m.map(&snap(&[TAB, L]), 0.05);
    assert_eq!(m.click_modes().0, "RT");
}

#[test]
fn click_stays_consumed_until_released_even_if_tab_is_let_go_first() {
    let mut m = mapper();
    m.map(&snap(&[TAB]), 0.0);
    m.map(&snap(&[TAB, L]), 0.01);
    let st = m.map(&snap(&[L]), 0.05);
    assert!(st.r2 == 0.0 && !st.buttons.contains(&Button::R1));
}

#[test]
fn toggle_reports_events_and_survives_a_profile_switch() {
    let mut m = mapper();
    toggle_click(&mut m, L, 0.0);
    assert_eq!(m.pop_events(), vec!["left click now acts as RB".to_string()]);
    assert!(m.pop_events().is_empty());
    m.set_profile("racing").unwrap();
    assert_eq!(m.click_modes().0, "RB");
}

#[test]
fn bumper_mode_applies_to_the_current_profiles_trigger_binding() {
    let mut m = mapper();
    m.set_profile("default").unwrap(); // the base profile has the click on R2
    toggle_click(&mut m, L, 0.0);
    assert!(m.map(&snap(&[L]), 1.0).buttons.contains(&Button::R1));
}

fn settle(m: &mut Mapper, frames: Vec<(f64, InputSnapshot)>) -> ControllerState {
    let mut last = ControllerState::default();
    for (t, f) in frames {
        last = m.map(&f, t);
    }
    last
}

#[test]
fn mouse_look_is_the_same_at_any_loop_rate() {
    let mut results = vec![];
    for hz in [125.0, 250.0, 1000.0] {
        let mut m = mapper_with(|c| c.mouse.smoothing = 0.6);
        let dt = 1.0 / hz;
        let frames = (0..(hz * 0.5) as usize).map(|i| (i as f64 * dt, snapd(&[], 2000.0 * dt, 0.0))).collect();
        results.push(settle(&mut m, frames).right_x);
    }
    let (mx, mn) = (results.iter().cloned().fold(f64::MIN, f64::max), results.iter().cloned().fold(f64::MAX, f64::min));
    assert!(mx - mn < 0.05, "{results:?}");
    assert!(results.iter().all(|r| *r > 0.5));
}

#[test]
fn mouse_look_recentres_when_the_mouse_stops() {
    let mut m = mapper();
    run(&mut m, &rep(snapd(&[], 3.0, 0.0), 30), 0.004, 0.0);
    let st = run(&mut m, &rep(snap(&[]), 40), 0.004, 1.0).pop().unwrap();
    assert!(st.right_x == 0.0 && st.right_y == 0.0);
}

#[test]
fn sensitivity_scales_the_stick_linearly() {
    let go = |s: f64| {
        let mut m = mapper_with(|c| c.mouse.sensitivity = s);
        settle(&mut m, (0..80).map(|i| (i as f64 * 0.004, snapd(&[], 2.0, 0.0))).collect()).right_x
    };
    let (lo, hi) = (go(0.02), go(0.04));
    assert!((hi - 2.0 * lo).abs() <= 0.05 * 2.0 * lo, "{lo} {hi}");
}

#[test]
fn command_clicks_fire_nothing_else() {
    let mut m = mapper_with(|c| c.output.min_press_ms = 0.0);
    let st = m.map(&snap(&[CMD, L]), 0.0);
    assert!(st.buttons.is_empty() && st.r2 == 0.0);
}

#[test]
fn adaptive_follows_the_click_even_if_the_click_is_rebound() {
    let mut c = user("[bindings]\nCIRCLE = [\"MOUSE_LEFT\"]\nR2 = []\n");
    c.output.min_press_ms = 0.0;
    let mut m = Mapper::new(c.clone(), None).unwrap();
    assert!(m.map(&snap(&[L]), 0.0).buttons.contains(&Button::CIRCLE));
    let mut m = Mapper::new(c, None).unwrap();
    let s = adaptive(&mut m, &[(snap(&[CMD, L]), 40)], 0.01);
    let last = s.last().unwrap();
    assert!(last.l2 > 0.2 && last.l2 < 0.5 && !last.buttons.contains(&Button::CIRCLE));
}

#[test]
fn the_pairing_is_not_a_config_option() {
    for key in ["left_click", "right_click"] {
        assert!(load_layers(Some(&format!("[adaptive_triggers]\n{key} = \"R2\"\n")), false).is_err());
    }
}

#[test]
fn a_click_held_before_command_does_not_leave_a_stuck_plain_trigger() {
    let mut m = mapper();
    run(&mut m, &rep(snap(&[L]), 20), 0.004, 0.0);
    let st = run(&mut m, &rep(snap(&[CMD, L]), 5), 0.004, 1.0).pop().unwrap();
    assert!(st.r2 == 0.0 && st.l2 > 0.0);
}

#[test]
fn bumper_mode_does_not_change_the_adaptive_triggers() {
    let mut m = mapper();
    m.map(&snap(&[TAB]), 0.0);
    m.map(&snap(&[TAB, L]), 0.01);
    m.map(&snap(&[]), 0.2);
    let s = adaptive(&mut m, &[(snap(&[CMD, L]), 40)], 0.01);
    let last = s.last().unwrap();
    assert!(last.l2 > 0.2 && last.l2 < 0.5 && !last.buttons.contains(&Button::R1));
}

// ---- config rules (spec 04 section 4) -------------------------------------------------------------
#[test]
fn reserved_and_tab_click_bindings_are_rejected() {
    assert!(load_layers(Some("[bindings]\nCROSS = [\"F10\"]\n"), false).is_err()); // hotkey
    assert!(load_layers(Some("[bindings]\nCROSS = [\"LEFT_COMMAND\"]\n"), false).is_err());
    assert!(load_layers(Some("[bindings]\nCROSS = [\"TAB+MOUSE_LEFT\"]\n"), false).is_err());
    assert!(load_layers(Some("toggle = \"F7\"\n"), false).is_err()); // overlaps profile_key
}

#[test]
fn migration_drops_now_reserved_bindings_with_notes() {
    let c = load_layers(Some("[bindings]\nCROSS = [\"F10\", \"Z\"]\nSQUARE = [\"F12\"]\n"), true).unwrap();
    assert_eq!(c.profile_bindings("default")["CROSS"], vec!["Z"]);
    assert_eq!(c.profile_bindings("default")["SQUARE"], vec!["Q"]); // shipped default fallback
    assert_eq!(c.notes.len(), 2);
}

#[test]
fn deleted_profiles_and_use_defaults_false() {
    let c = user("deleted_profiles = [\"racing\"]\n");
    assert_eq!(c.profile_names(), vec!["default", "assassins_creed"]);
}

#[test]
fn share_is_an_alias_of_create() {
    let c = user("[bindings]\nSHARE = [\"N\"]\n");
    assert_eq!(c.profile_bindings("default")["CREATE"], vec!["N"]);
}
