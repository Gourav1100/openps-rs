use openps_input::keys::{Input, Key};
use openps_input::mapping::Mapper;
use openps_input::{load_layers, InputSnapshot};

// a user key map like the one this was reported with: racing keys on the arrow keys, Option+arrows for the camera
const USER: &str = "toggle = \"F10\"\nquit = \"F12\"\nprofile_key = \"F7\"\n[profiles.racing.bindings]\nCROSS = [\"X\", \"EQUAL\"]\nR2 = [\"UP_ARROW\"]\nRIGHT_STICK_UP = [\"LEFT_OPTION+UP_ARROW\"]\n";

fn out(profile: &str, keys: &[Key]) -> openps_input::state::ControllerState {
    let cfg = load_layers(Some(USER), true).unwrap();
    let mut m = Mapper::new(cfg, Some(profile)).unwrap();
    let snap = InputSnapshot::new(keys.iter().map(|k| Input::Key(*k)), 0.0, 0.0);
    m.map(&snap, 0.0)
}

#[test]
fn pressing_f10_alone_changes_nothing_in_any_profile() {
    for p in ["default", "racing", "assassins_creed"] {
        let idle = out(p, &[]);
        let f10 = out(p, &[Key::F10]);
        assert_eq!(idle, f10, "profile {p}: F10 changed the pad state: {f10:?}");
    }
}
