//! The scene filter: it combines what the picture says (per-frame probabilities from the model) with what the player did (actions) into a
//! steady belief about the scene.
//!
//! Context is one of on foot, in a vehicle, in a menu or in a cutscene; the view is first or third person. Both are forward (HMM) filters:
//! the frame probabilities are the observation, and the actions only change how likely a change is. A press of the enter/exit action makes
//! on foot <-> vehicle likely for a few seconds, the pause action does the same for game <-> menu, the camera action for first <-> third
//! person. With no press the state is held. Cutscenes start and end on their own, so they have their own rates. An action that no
//! control carries cannot be waited for, so its changes fall back to the plain rate of a smoother.
//!
//! The numbers are the ones `tools/scene-context/scenelab/filtersim.py` was evaluated with (`tests/reference.rs` checks this port against it).

use openps_input::scene::{Action, SceneContext, SceneView};

const FOOT: usize = 0;
const VEH: usize = 1;
const MENU: usize = 2;
const CUT: usize = 3;

#[derive(Clone, Debug)]
pub struct Params {
    /// How far one frame is trusted per second between frames (capped at 1): neighbouring frames say nearly the same thing.
    pub alpha_per_s: f64,
    /// The trust of the first frame.
    pub alpha_first: f64,
    /// Change per second between two states the player triggers, when its action is available.
    pub base_cued: f64,
    /// The same when it is not (a plain smoother).
    pub base_free: f64,
    /// Extra change per second while an action is fresh.
    pub cue_rate: f64,
    pub cutscene_in: f64,
    pub cutscene_out: f64,
    pub menu_to_cutscene: f64,
    pub view_base_cued: f64,
    pub view_base_free: f64,
    pub view_cue_rate: f64,
    /// A vehicle's camera is separate from the on-foot one, so entering or leaving can change the view too.
    pub view_after_vehicle: f64,
    /// Seconds an action stays fresh.
    pub vehicle_cue_s: f64,
    pub pause_cue_s: f64,
    pub camera_cue_s: f64,
    /// On foot and in a vehicle change by the enter/exit press, not by the picture (while that action is available): a vehicle beside
    /// you looks like one you are in, so no amount of such pictures moves the state. Off, the press only makes a change likely.
    pub vehicle_by_press: bool,
    /// The share of an on-foot belief that an enter/exit press moves to "in a vehicle": the press may not have entered anything.
    pub enter_toggle: f64,
    /// The share of an in-vehicle belief that moves to "on foot" at the press: leaving a vehicle always works.
    pub exit_toggle: f64,
    /// Seconds after leaving a vehicle in which the picture says nothing about on foot against in a vehicle (the vehicle is still beside you).
    pub exit_lock_s: f64,
    /// Change per second from on foot to in a vehicle with no press (a cutscene that puts you in a car): practically never, but not never,
    /// so a long run of vehicle pictures puts a wrong state right.
    pub foot_to_vehicle_free: f64,
    /// Seconds after an enter press in which the next presses are more tries at getting in, not a way out, until the pictures show the
    /// vehicle (each press renews it). Pressing again and again to get in must not undo the entry once it works.
    pub enter_retry_s: f64,
    /// The belief in "in a vehicle" at which an entry counts as done, and a press is a way out again.
    pub enter_confirm: f64,
    /// Seconds after a press that left a vehicle in which the next presses are the same exit pressed again, not a way back in (each press
    /// renews it; the pictures cannot confirm an exit, the vehicle is still beside you).
    pub exit_retry_s: f64,
}

impl Params {
    /// The filter as `tools/scene-context/scenelab/filtersim.py` first had it: the press only raises the odds of a change.
    pub fn legacy() -> Self {
        Self { vehicle_by_press: false, ..Self::default() }
    }
}

impl Default for Params {
    fn default() -> Self {
        Self {
            alpha_per_s: 0.35,
            alpha_first: 0.7,
            base_cued: 0.003,
            base_free: 0.02,
            cue_rate: 0.6,
            cutscene_in: 0.01,
            cutscene_out: 0.03,
            menu_to_cutscene: 0.003,
            view_base_cued: 0.002,
            view_base_free: 0.02,
            view_cue_rate: 1.0,
            view_after_vehicle: 0.05,
            vehicle_cue_s: 5.0,
            pause_cue_s: 5.0,
            camera_cue_s: 3.0,
            vehicle_by_press: true,
            enter_toggle: 0.5,
            exit_toggle: 1.0 - 1e-9,
            exit_lock_s: 12.0,
            foot_to_vehicle_free: 1e-5,
            enter_retry_s: 2.0,
            enter_confirm: 0.8,
            exit_retry_s: 2.0,
        }
    }
}

/// What the filter believes after an update.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Estimate {
    pub context: SceneContext,
    /// The belief in `context`, 0 to 1.
    pub confidence: f32,
    /// `None` in a menu or cutscene, where there is no camera to speak of.
    pub view: Option<SceneView>,
}

pub struct SceneFilter {
    params: Params,
    context: [f64; 4],
    view: [f64; 2],
    last_t: Option<f64>,
    actions: Vec<(f64, Action)>,
    /// Enter/exit presses not yet applied to the belief.
    pending: Vec<f64>,
    /// Until then the picture says nothing about on foot against in a vehicle.
    lock_until: f64,
    /// The time of the latest press that tried to get in, until a picture of the vehicle confirms it (or the retries stop).
    entering: Option<f64>,
    /// The time of the latest press that left a vehicle.
    left_at: f64,
    /// On foot against in a vehicle as they were when gameplay was last the likely scene: what a menu or cutscene gives back.
    resume: [f64; 2],
    available: [bool; 3],
    /// Scenes the key map has a layer for (on foot, in a vehicle, menu, cutscene); the rest are never believed.
    allowed: [bool; 4],
}

fn index(a: Action) -> usize {
    match a {
        Action::EnterOrExitVehicle => 0,
        Action::ChangeCamera => 1,
        Action::PauseGame => 2,
    }
}

impl SceneFilter {
    pub fn new(params: Params) -> Self {
        Self {
            params,
            context: [0.25; 4],
            view: [0.5; 2],
            last_t: None,
            actions: vec![],
            pending: vec![],
            lock_until: f64::NEG_INFINITY,
            entering: None,
            left_at: f64::NEG_INFINITY,
            resume: [0.5; 2],
            available: [false; 3],
            allowed: [true; 4],
        }
    }

    /// The full belief: context (on foot, in vehicle, menu, cutscene) and view (first, third).
    pub fn belief(&self) -> ([f64; 4], [f64; 2]) {
        (self.context, self.view)
    }

    /// Forget everything (detection was switched on again, or the stream restarted).
    pub fn reset(&mut self) {
        let p = self.params.clone();
        *self = Self::new(p);
    }

    /// Which actions some control carries right now. Changes the filter would wait an action for are plain smoothing without it.
    pub fn set_available(&mut self, action: Action, on: bool) {
        self.available[index(action)] = on;
    }

    /// Which scenes may be believed: a game without vehicles has no use for "in a vehicle", and its vehicle-like pictures then count as
    /// on foot. Nothing allowed means everything is.
    pub fn set_allowed(&mut self, allowed: [bool; 4]) {
        self.allowed = if allowed.iter().any(|a| *a) { allowed } else { [true; 4] };
    }

    pub fn push_action(&mut self, t: f64, action: Action) {
        self.actions.push((t, action));
        if action == Action::EnterOrExitVehicle {
            self.pending.push(t);
        }
    }

    /// The enter/exit action decides on foot against in a vehicle.
    fn by_press(&self) -> bool {
        self.params.vehicle_by_press && self.available[index(Action::EnterOrExitVehicle)]
    }

    /// A press moves belief between on foot and in a vehicle, in the direction the likelier state allows: out of a vehicle (leaving works),
    /// or into one (maybe: the press may not have entered anything).
    fn apply_press(&mut self, at: f64) {
        let p = &self.params;
        if at - self.left_at <= p.exit_retry_s {
            self.left_at = at;
            self.lock_until = at + p.exit_lock_s;
            return;
        }
        // still getting in: the vehicle is not confirmed, so this is another try, never the way out. It only brings the belief back up to
        // what a first press gives (the first try may have faded), so a run of tries cannot make the vehicle likelier than one does.
        if self.entering.is_some_and(|since| at - since <= p.enter_retry_s) {
            let (foot, veh) = (self.context[FOOT], self.context[VEH]);
            let moved = (p.enter_toggle * (foot + veh) - veh).max(0.0);
            self.context[FOOT] = foot - moved;
            self.context[VEH] = veh + moved;
            self.entering = Some(at);
            return;
        }
        let (foot, veh) = (self.context[FOOT], self.context[VEH]);
        let leaving = veh > foot;
        let moved = if leaving { veh * p.exit_toggle } else { -foot * p.enter_toggle };
        self.context[FOOT] = foot + moved;
        self.context[VEH] = veh - moved;
        // the vehicle that was left is still beside you: its picture must not take you back in
        self.lock_until = if leaving { at + p.exit_lock_s } else { f64::NEG_INFINITY };
        self.entering = (!leaving).then_some(at);
        if leaving {
            self.left_at = at;
        }
    }

    fn cue_len(&self, a: Action) -> f64 {
        match a {
            Action::EnterOrExitVehicle => self.params.vehicle_cue_s,
            Action::PauseGame => self.params.pause_cue_s,
            Action::ChangeCamera => self.params.camera_cue_s,
        }
    }

    /// Seconds of `[t0, t1]` covered by the windows that follow `action` presses.
    fn armed(&self, action: Action, t0: f64, t1: f64) -> f64 {
        let len = self.cue_len(action);
        let mut windows: Vec<(f64, f64)> = self
            .actions
            .iter()
            .filter(|(p, a)| *a == action && *p < t1 && p + len > t0)
            .map(|(p, _)| (*p, p + len))
            .collect();
        windows.sort_by(|a, b| a.0.total_cmp(&b.0));
        let (mut total, mut end) = (0.0, t0);
        for (a, b) in windows {
            let (a, b) = (a.max(end), b.min(t1));
            if b > a {
                total += b - a;
                end = b;
            }
        }
        total
    }

    /// One observation: `context` is the model's probability of on foot, in vehicle, menu, cutscene; `view` of first and third person.
    /// `t` is in seconds on the clock the actions use.
    pub fn update(&mut self, t: f64, context: [f32; 4], view: [f32; 2]) -> Estimate {
        let dt = self.last_t.map(|l| (t - l).max(0.0));
        let alpha = dt.map_or(self.params.alpha_first, |dt| (self.params.alpha_per_s * dt).min(1.0));
        if let (Some(last), Some(dt)) = (self.last_t, dt) {
            let h = self.context_hazard(last, t, dt);
            self.context = step(&self.context, &h);
            let hv = self.view_hazard(last, t, dt);
            self.view = step(&self.view, &hv);
        }
        if self.by_press() {
            self.pending.sort_by(f64::total_cmp);
            let due = self.pending.iter().take_while(|p| **p <= t).count();
            for at in self.pending.drain(..due).collect::<Vec<_>>() {
                self.apply_press(at);
            }
        } else {
            self.pending.clear();
        }
        self.last_t = Some(t);
        let horizon = self.params.vehicle_cue_s.max(self.params.pause_cue_s).max(self.params.camera_cue_s);
        self.actions.retain(|(p, _)| p + horizon > t);

        let mut seen = context.map(f64::from);
        let restricted = self.allowed.iter().any(|a| !a);
        if restricted {
            let kept: f64 = (0..4).filter(|i| self.allowed[*i]).map(|i| seen[i]).sum();
            for (p, allowed) in seen.iter_mut().zip(self.allowed) {
                *p = if !allowed {
                    0.0
                } else if kept > 1e-6 {
                    *p / kept
                } else {
                    1.0
                };
            }
        }
        if self.by_press() && t < self.lock_until {
            let both = (seen[FOOT] + seen[VEH]) / 2.0;
            (seen[FOOT], seen[VEH]) = (both, both);
        }
        observe(&mut self.context, seen, alpha);
        if restricted {
            self.forbid();
        }
        if self.entering.is_some() && self.context[VEH] >= self.params.enter_confirm {
            self.entering = None;
        }
        let game = self.context[FOOT] + self.context[VEH];
        if game > 0.5 {
            self.resume = [self.context[FOOT] / game, self.context[VEH] / game];
        }
        let ctx = argmax(&self.context);
        if ctx == FOOT || ctx == VEH {
            observe(&mut self.view, view.map(f64::from), alpha);
        }
        Estimate {
            context: SceneContext::ALL[ctx],
            confidence: self.context[ctx] as f32,
            view: (ctx == FOOT || ctx == VEH).then(|| {
                if argmax(&self.view) == 0 {
                    SceneView::FirstPerson
                } else {
                    SceneView::ThirdPerson
                }
            }),
        }
    }

    /// Take the belief out of the scenes that are not allowed (a press or a hazard may have put some there).
    fn forbid(&mut self) {
        for i in 0..4 {
            if !self.allowed[i] {
                self.context[i] = 0.0;
            }
        }
        let sum: f64 = self.context.iter().sum();
        if sum > 1e-12 {
            self.context.iter_mut().for_each(|b| *b /= sum);
        } else {
            let n = self.allowed.iter().filter(|a| **a).count() as f64;
            for i in 0..4 {
                self.context[i] = if self.allowed[i] { 1.0 / n } else { 0.0 };
            }
        }
    }

    fn context_hazard(&self, t0: f64, t1: f64, dt: f64) -> [[f64; 4]; 4] {
        let p = &self.params;
        let rate = |on: bool| if on { p.base_cued } else { p.base_free };
        let vehicle = rate(self.available[index(Action::EnterOrExitVehicle)]);
        let pause = rate(self.available[index(Action::PauseGame)]);
        let mut h = [[0.0; 4]; 4];
        h[FOOT][VEH] = vehicle * dt;
        h[VEH][FOOT] = vehicle * dt;
        for g in [FOOT, VEH] {
            h[g][MENU] = pause * dt;
            h[MENU][g] = pause * dt;
            h[g][CUT] = p.cutscene_in * dt;
            h[CUT][g] = p.cutscene_out * dt;
        }
        h[MENU][CUT] = p.menu_to_cutscene * dt;
        h[CUT][MENU] = p.cutscene_out * dt;
        if self.available[index(Action::EnterOrExitVehicle)] {
            if self.by_press() {
                // the press already moved the belief; from then on it is the pictures that confirm or undo it
                h[FOOT][VEH] = p.foot_to_vehicle_free * dt;
            } else {
                let a = p.cue_rate * self.armed(Action::EnterOrExitVehicle, t0, t1);
                h[FOOT][VEH] += a;
                h[VEH][FOOT] += a;
            }
        }
        if self.available[index(Action::PauseGame)] {
            let a = p.cue_rate * self.armed(Action::PauseGame, t0, t1);
            for g in [FOOT, VEH] {
                h[g][MENU] += a;
                h[MENU][g] += a;
            }
        }
        if self.by_press() {
            for (i, g) in [FOOT, VEH].into_iter().enumerate() {
                h[MENU][g] *= 2.0 * self.resume[i];
                h[CUT][g] *= 2.0 * self.resume[i];
            }
        }
        h
    }

    fn view_hazard(&self, t0: f64, t1: f64, dt: f64) -> [[f64; 2]; 2] {
        let p = &self.params;
        let camera = self.available[index(Action::ChangeCamera)];
        let mut rate = if camera { p.view_base_cued } else { p.view_base_free } * dt;
        if camera {
            rate += p.view_cue_rate * self.armed(Action::ChangeCamera, t0, t1);
        }
        if self.available[index(Action::EnterOrExitVehicle)] {
            rate += p.view_after_vehicle * self.armed(Action::EnterOrExitVehicle, t0, t1);
        }
        [[0.0, rate], [rate, 0.0]]
    }
}

fn argmax(v: &[f64]) -> usize {
    v.iter().enumerate().fold(0, |best, (i, x)| if *x > v[best] { i } else { best })
}

/// Move the belief by the accumulated hazard `h[i][j]` (rate times seconds) of going from i to j.
fn step<const N: usize>(belief: &[f64; N], h: &[[f64; N]; N]) -> [f64; N] {
    let mut out = [0.0; N];
    for i in 0..N {
        let mut row = h[i];
        row[i] = 0.0;
        let total: f64 = row.iter().sum();
        let stay = (-total).exp();
        out[i] += belief[i] * stay;
        if total > 0.0 {
            for j in 0..N {
                out[j] += belief[i] * (1.0 - stay) * row[j] / total;
            }
        }
    }
    out
}

fn observe<const N: usize>(belief: &mut [f64; N], p: [f64; N], alpha: f64) {
    for i in 0..N {
        belief[i] *= p[i].max(1e-6).powf(alpha);
    }
    let sum: f64 = belief.iter().sum();
    for b in belief.iter_mut() {
        *b /= sum;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FOOT_P: [f32; 4] = [0.8, 0.1, 0.05, 0.05];
    const VEH_P: [f32; 4] = [0.1, 0.8, 0.05, 0.05];
    const MENU_P: [f32; 4] = [0.05, 0.05, 0.8, 0.1];
    const CUT_P: [f32; 4] = [0.1, 0.05, 0.05, 0.8];
    const FIRST: [f32; 2] = [0.8, 0.2];
    const THIRD: [f32; 2] = [0.2, 0.8];

    fn all_available() -> SceneFilter {
        let mut f = SceneFilter::new(Params::default());
        for a in Action::ALL {
            f.set_available(a, true);
        }
        f
    }

    /// Feed `n` frames, one every `dt` seconds from `*t`, and return the last estimate.
    fn run(f: &mut SceneFilter, t: &mut f64, n: usize, dt: f64, ctx: [f32; 4], view: [f32; 2]) -> Estimate {
        let mut last = None;
        for _ in 0..n {
            *t += dt;
            last = Some(f.update(*t, ctx, view));
        }
        last.unwrap()
    }

    #[test]
    fn steady_evidence_settles_on_the_state() {
        let mut f = all_available();
        let mut t = 0.0;
        let e = run(&mut f, &mut t, 6, 1.0, VEH_P, THIRD);
        assert_eq!(e.context, SceneContext::InVehicle);
        assert!(e.confidence > 0.8, "{}", e.confidence);
        assert_eq!(e.view, Some(SceneView::ThirdPerson));
    }

    #[test]
    fn without_an_action_one_odd_frame_does_not_change_the_state() {
        let mut f = all_available();
        let mut t = 0.0;
        run(&mut f, &mut t, 8, 1.0, FOOT_P, THIRD);
        let e = run(&mut f, &mut t, 1, 1.0, VEH_P, THIRD);
        assert_eq!(e.context, SceneContext::OnFoot);
        let e = run(&mut f, &mut t, 1, 1.0, FOOT_P, THIRD);
        assert_eq!(e.context, SceneContext::OnFoot);
    }

    #[test]
    fn the_enter_action_lets_the_first_vehicle_frame_switch_the_state() {
        let mut f = all_available();
        let mut t = 0.0;
        run(&mut f, &mut t, 8, 2.0, FOOT_P, THIRD);
        f.push_action(t + 0.5, Action::EnterOrExitVehicle);
        let e = run(&mut f, &mut t, 1, 2.0, VEH_P, THIRD);
        assert_eq!(e.context, SceneContext::InVehicle);
    }

    #[test]
    fn an_old_action_no_longer_helps() {
        let mut f = all_available();
        let mut t = 0.0;
        run(&mut f, &mut t, 8, 1.0, FOOT_P, THIRD);
        f.push_action(t, Action::EnterOrExitVehicle);
        run(&mut f, &mut t, 10, 1.0, FOOT_P, THIRD); // the press led to nothing
        let e = run(&mut f, &mut t, 1, 1.0, VEH_P, THIRD);
        assert_eq!(e.context, SceneContext::OnFoot);
    }

    #[test]
    fn the_wrong_action_does_not_help() {
        let mut f = all_available();
        let mut t = 0.0;
        run(&mut f, &mut t, 8, 1.0, FOOT_P, THIRD);
        f.push_action(t + 0.5, Action::ChangeCamera);
        let e = run(&mut f, &mut t, 1, 1.0, VEH_P, THIRD);
        assert_eq!(e.context, SceneContext::OnFoot);
    }

    #[test]
    fn without_a_labeled_control_it_is_a_plain_smoother() {
        let mut f = SceneFilter::new(Params::default());
        let mut t = 0.0;
        run(&mut f, &mut t, 8, 2.0, FOOT_P, THIRD);
        let e = run(&mut f, &mut t, 1, 2.0, VEH_P, THIRD);
        assert_eq!(e.context, SceneContext::OnFoot, "one frame is not enough");
        let e = run(&mut f, &mut t, 2, 2.0, VEH_P, THIRD);
        assert_eq!(e.context, SceneContext::InVehicle, "a few frames are");
        // the same frames with an available but unused action are doubted for longer
        let mut g = all_available();
        let mut t = 0.0;
        run(&mut g, &mut t, 8, 2.0, FOOT_P, THIRD);
        let e = run(&mut g, &mut t, 3, 2.0, VEH_P, THIRD);
        assert_eq!(e.context, SceneContext::OnFoot);
    }

    #[test]
    fn a_pause_action_opens_the_menu_and_a_cutscene_needs_none() {
        let mut f = all_available();
        let mut t = 0.0;
        run(&mut f, &mut t, 8, 2.0, FOOT_P, THIRD);
        let e = run(&mut f, &mut t, 1, 2.0, MENU_P, THIRD);
        assert_eq!(e.context, SceneContext::OnFoot, "a menu without a pause press is doubted");
        let mut f = all_available();
        let mut t = 0.0;
        run(&mut f, &mut t, 8, 2.0, FOOT_P, THIRD);
        f.push_action(t + 0.5, Action::PauseGame);
        let e = run(&mut f, &mut t, 1, 2.0, MENU_P, THIRD);
        assert_eq!(e.context, SceneContext::Menu);
        assert_eq!(e.view, None, "no camera in a menu");
        let mut f = all_available();
        let mut t = 0.0;
        run(&mut f, &mut t, 8, 2.0, FOOT_P, THIRD);
        let e = run(&mut f, &mut t, 1, 2.0, CUT_P, THIRD);
        assert_eq!(e.context, SceneContext::OnFoot, "one cutscene frame is not enough");
        let e = run(&mut f, &mut t, 3, 2.0, CUT_P, THIRD);
        assert_eq!(e.context, SceneContext::Cutscene, "a cutscene needs no press");
    }

    #[test]
    fn the_camera_action_switches_the_view_and_nothing_else_does_quickly() {
        let mut f = all_available();
        let mut t = 0.0;
        run(&mut f, &mut t, 8, 1.0, FOOT_P, THIRD);
        let e = run(&mut f, &mut t, 1, 1.0, FOOT_P, FIRST);
        assert_eq!(e.view, Some(SceneView::ThirdPerson), "an odd view frame is doubted");
        f.push_action(t + 0.5, Action::ChangeCamera);
        let e = run(&mut f, &mut t, 1, 1.0, FOOT_P, FIRST);
        assert_eq!(e.view, Some(SceneView::FirstPerson));
    }

    #[test]
    fn the_belief_stays_a_distribution_whatever_it_is_fed() {
        let mut f = all_available();
        let mut t = 0.0;
        for i in 0..200 {
            t += 0.3 + (i % 7) as f64;
            let p = [(i % 3) as f32 / 3.0, 0.0, 1.0, (i % 5) as f32];
            f.push_action(t - 0.1, Action::ALL[i % 3]);
            let e = f.update(t, p, [0.0, 1.0]);
            assert!(e.confidence.is_finite() && (0.0..=1.0001).contains(&e.confidence));
        }
        // time going backwards (a restarted clock) must not break it either
        let e = f.update(1.0, FOOT_P, FIRST);
        assert!(e.confidence.is_finite());
    }

    #[test]
    fn a_vehicle_beside_you_does_not_put_you_in_it() {
        let mut f = all_available();
        let mut t = 0.0;
        run(&mut f, &mut t, 150, 0.5, FOOT_P, THIRD); // over a minute on foot
                                                      // standing next to a car: the pictures say vehicle, nothing was pressed
        let e = run(&mut f, &mut t, 16, 0.5, VEH_P, THIRD);
        assert_eq!(e.context, SceneContext::OnFoot, "eight seconds of it change nothing");
        // pictures that stay that way for minutes do put a wrong state right
        let e = run(&mut f, &mut t, 600, 0.5, VEH_P, THIRD);
        assert_eq!(e.context, SceneContext::InVehicle);
    }

    #[test]
    fn a_menu_or_cutscene_is_no_way_around_the_press() {
        let mut f = all_available();
        let mut t = 0.0;
        run(&mut f, &mut t, 150, 0.5, FOOT_P, THIRD);
        run(&mut f, &mut t, 20, 0.5, CUT_P, THIRD); // a cutscene next to a car
        let e = run(&mut f, &mut t, 20, 0.5, VEH_P, THIRD);
        assert_eq!(e.context, SceneContext::OnFoot, "it comes back on foot, as it went in");
        // and the other way round
        let mut f = all_available();
        let mut t = 0.0;
        run(&mut f, &mut t, 150, 0.5, VEH_P, THIRD);
        run(&mut f, &mut t, 20, 0.5, MENU_P, THIRD);
        let e = run(&mut f, &mut t, 40, 0.5, VEH_P, THIRD);
        assert_eq!(e.context, SceneContext::InVehicle, "and back in the vehicle");
    }

    #[test]
    fn leaving_a_vehicle_works_even_if_the_vehicle_is_still_beside_you() {
        let mut f = all_available();
        let mut t = 0.0;
        run(&mut f, &mut t, 150, 0.5, VEH_P, THIRD);
        f.push_action(t + 0.2, Action::EnterOrExitVehicle);
        let e = run(&mut f, &mut t, 1, 0.5, VEH_P, THIRD);
        assert_eq!(e.context, SceneContext::OnFoot, "the first picture after the press already says vehicle");
        // the vehicle is in every picture from now on
        let e = run(&mut f, &mut t, 40, 0.5, VEH_P, THIRD);
        assert_eq!(e.context, SceneContext::OnFoot);
    }

    #[test]
    fn entering_is_confirmed_by_the_pictures_and_an_empty_press_is_undone() {
        let mut f = all_available();
        let mut t = 0.0;
        run(&mut f, &mut t, 10, 0.5, FOOT_P, THIRD);
        f.push_action(t + 0.2, Action::EnterOrExitVehicle);
        let e = run(&mut f, &mut t, 2, 0.5, VEH_P, FIRST);
        assert_eq!(e.context, SceneContext::InVehicle);
        // a press that entered nothing: the pictures stay on foot
        let mut f = all_available();
        let mut t = 0.0;
        run(&mut f, &mut t, 10, 0.5, FOOT_P, THIRD);
        f.push_action(t + 0.2, Action::EnterOrExitVehicle);
        let e = run(&mut f, &mut t, 3, 0.5, FOOT_P, THIRD);
        assert_eq!(e.context, SceneContext::OnFoot);
    }

    #[test]
    fn stopping_halfway_into_a_vehicle_leaves_you_on_foot() {
        // the press, then pictures that are mostly the vehicle beside you (the door, the camera swinging) for a few seconds, then
        // pictures of walking away: the vehicle layer must never be picked on the way
        const BESIDE: [f32; 4] = [0.5, 0.4, 0.05, 0.05];
        let mut f = all_available();
        let mut t = 0.0;
        run(&mut f, &mut t, 10, 0.5, FOOT_P, THIRD);
        f.push_action(t + 0.2, Action::EnterOrExitVehicle);
        for _ in 0..8 {
            let e = run(&mut f, &mut t, 1, 0.5, BESIDE, THIRD);
            assert_eq!(e.context, SceneContext::OnFoot, "entering has not been confirmed by a picture of being inside");
        }
        for _ in 0..8 {
            let e = run(&mut f, &mut t, 1, 0.5, FOOT_P, THIRD);
            assert_eq!(e.context, SceneContext::OnFoot);
        }
    }

    #[test]
    fn getting_in_is_confirmed_within_a_couple_of_seconds_of_sitting_down() {
        const BESIDE: [f32; 4] = [0.5, 0.4, 0.05, 0.05];
        let mut f = all_available();
        let mut t = 0.0;
        run(&mut f, &mut t, 10, 0.5, FOOT_P, THIRD);
        f.push_action(t + 0.2, Action::EnterOrExitVehicle);
        run(&mut f, &mut t, 6, 0.5, BESIDE, THIRD);
        let mut seated = None;
        for i in 0..10 {
            let e = run(&mut f, &mut t, 1, 0.5, VEH_P, FIRST);
            if e.context == SceneContext::InVehicle {
                seated = Some(i);
                break;
            }
        }
        assert!(seated.is_some_and(|i| i < 5), "in the vehicle after {seated:?} pictures of sitting in it");
    }

    #[test]
    fn pressing_again_and_again_to_get_in_does_not_undo_the_entry() {
        const BESIDE: [f32; 4] = [0.5, 0.4, 0.05, 0.05];
        let mut f = all_available();
        let mut t = 0.0;
        run(&mut f, &mut t, 10, 0.5, FOOT_P, THIRD);
        // four presses in two seconds while the door opens: only the first is a change, the rest are tries
        for i in 0..4 {
            f.push_action(t + 0.2 + 0.5 * f64::from(i), Action::EnterOrExitVehicle);
        }
        for _ in 0..6 {
            let e = run(&mut f, &mut t, 1, 0.5, BESIDE, THIRD);
            assert_eq!(e.context, SceneContext::OnFoot, "not in yet");
        }
        for _ in 0..12 {
            run(&mut f, &mut t, 1, 0.5, VEH_P, FIRST);
        }
        let e = run(&mut f, &mut t, 1, 0.5, VEH_P, FIRST);
        assert_eq!(e.context, SceneContext::InVehicle, "the extra presses did not take it back out");
        // once the vehicle is confirmed a press is the way out again
        f.push_action(t + 0.2, Action::EnterOrExitVehicle);
        let e = run(&mut f, &mut t, 1, 0.5, VEH_P, FIRST);
        assert_eq!(e.context, SceneContext::OnFoot);
    }

    #[test]
    fn tries_that_get_nowhere_leave_you_on_foot() {
        let mut f = all_available();
        let mut t = 0.0;
        run(&mut f, &mut t, 10, 0.5, FOOT_P, THIRD);
        for i in 0..6 {
            f.push_action(t + 0.2 + f64::from(i), Action::EnterOrExitVehicle);
        }
        let e = run(&mut f, &mut t, 40, 0.5, FOOT_P, THIRD);
        assert_eq!(e.context, SceneContext::OnFoot);
        // the entry that finally works, a few seconds after a press that did nothing
        f.push_action(t + 0.2, Action::EnterOrExitVehicle);
        let e = run(&mut f, &mut t, 4, 0.5, VEH_P, FIRST);
        assert_eq!(e.context, SceneContext::InVehicle);
        let mut f = all_available();
        let mut t = 0.0;
        run(&mut f, &mut t, 10, 0.5, FOOT_P, THIRD);
        f.push_action(t + 0.2, Action::EnterOrExitVehicle);
        run(&mut f, &mut t, 8, 0.5, FOOT_P, THIRD); // nothing happened
        f.push_action(t + 0.2, Action::EnterOrExitVehicle);
        let e = run(&mut f, &mut t, 4, 0.5, VEH_P, FIRST);
        assert_eq!(e.context, SceneContext::InVehicle, "the second try still counts");
    }

    #[test]
    fn leaving_pressed_twice_is_one_exit() {
        let mut f = all_available();
        let mut t = 0.0;
        run(&mut f, &mut t, 150, 0.5, VEH_P, THIRD);
        f.push_action(t + 0.2, Action::EnterOrExitVehicle);
        f.push_action(t + 0.9, Action::EnterOrExitVehicle);
        let e = run(&mut f, &mut t, 4, 0.5, VEH_P, THIRD);
        assert_eq!(e.context, SceneContext::OnFoot);
        // presses up to two seconds apart are still the same exit
        for _ in 0..3 {
            f.push_action(t + 0.2, Action::EnterOrExitVehicle);
            let e = run(&mut f, &mut t, 3, 0.5, VEH_P, THIRD);
            assert_eq!(e.context, SceneContext::OnFoot, "the same exit pressed again");
        }
        // after a longer pause a press is a new one: on foot, so it tries to get in
        run(&mut f, &mut t, 30, 0.5, FOOT_P, THIRD);
        f.push_action(t + 0.2, Action::EnterOrExitVehicle);
        let e = run(&mut f, &mut t, 4, 0.5, VEH_P, FIRST);
        assert_eq!(e.context, SceneContext::InVehicle);
    }

    #[test]
    fn a_scene_that_is_not_allowed_is_never_believed() {
        let mut f = all_available();
        f.set_allowed([true, false, true, true]); // a game without vehicles
        let mut t = 0.0;
        run(&mut f, &mut t, 10, 0.5, FOOT_P, THIRD);
        f.push_action(t + 0.2, Action::EnterOrExitVehicle);
        for _ in 0..40 {
            let e = run(&mut f, &mut t, 1, 0.5, VEH_P, FIRST);
            assert_eq!(e.context, SceneContext::OnFoot, "vehicle-like pictures count as on foot");
        }
        let (ctx, _) = f.belief();
        assert_eq!(ctx[VEH], 0.0);
        assert!((ctx.iter().sum::<f64>() - 1.0).abs() < 1e-9);
        // menus still work
        let e = run(&mut f, &mut t, 20, 0.5, MENU_P, THIRD);
        assert_eq!(e.context, SceneContext::Menu);
        // no menu layer: a menu picture is on foot
        f.set_allowed([true, true, false, false]);
        let e = run(&mut f, &mut t, 20, 0.5, MENU_P, THIRD);
        assert!(matches!(e.context, SceneContext::OnFoot | SceneContext::InVehicle));
        // nothing allowed = everything allowed
        f.set_allowed([false; 4]);
        let e = run(&mut f, &mut t, 40, 0.5, MENU_P, THIRD);
        assert_eq!(e.context, SceneContext::Menu);
    }

    #[test]
    fn on_foot_pictures_take_you_out_of_a_vehicle_without_a_press() {
        // knocked off a bike, or the vehicle destroyed: no press, but the pictures are not of a vehicle any more
        let mut f = all_available();
        let mut t = 0.0;
        run(&mut f, &mut t, 10, 0.5, VEH_P, THIRD);
        let e = run(&mut f, &mut t, 30, 0.5, FOOT_P, THIRD);
        assert_eq!(e.context, SceneContext::OnFoot);
    }

    #[test]
    fn a_press_that_arrives_after_the_picture_waits_for_the_next_one() {
        let mut f = all_available();
        let mut t = 0.0;
        run(&mut f, &mut t, 10, 0.5, VEH_P, THIRD);
        f.push_action(t + 0.9, Action::EnterOrExitVehicle); // after the next picture's time
        let e = run(&mut f, &mut t, 1, 0.5, VEH_P, THIRD);
        assert_eq!(e.context, SceneContext::InVehicle);
        let e = run(&mut f, &mut t, 1, 0.5, VEH_P, THIRD);
        assert_eq!(e.context, SceneContext::OnFoot);
    }

    #[test]
    fn the_legacy_filter_still_lets_pictures_change_the_state() {
        let mut f = SceneFilter::new(Params::legacy());
        for a in Action::ALL {
            f.set_available(a, true);
        }
        let mut t = 0.0;
        run(&mut f, &mut t, 10, 0.5, FOOT_P, THIRD);
        let e = run(&mut f, &mut t, 60, 0.5, VEH_P, THIRD);
        assert_eq!(e.context, SceneContext::InVehicle);
    }

    #[test]
    fn reset_forgets_the_scene() {
        let mut f = all_available();
        let mut t = 0.0;
        run(&mut f, &mut t, 8, 1.0, VEH_P, THIRD);
        f.reset();
        let e = f.update(1.0, FOOT_P, THIRD);
        assert_eq!(e.context, SceneContext::OnFoot);
    }

    #[test]
    fn armed_adds_overlapping_windows_once() {
        let mut f = all_available();
        f.push_action(10.0, Action::PauseGame);
        f.push_action(12.0, Action::PauseGame);
        // windows [10,15] and [12,17] cover [10,17]; the question covers [9,14]
        assert!((f.armed(Action::PauseGame, 9.0, 14.0) - 4.0).abs() < 1e-9);
        assert!((f.armed(Action::PauseGame, 9.0, 20.0) - 7.0).abs() < 1e-9);
        assert_eq!(f.armed(Action::ChangeCamera, 9.0, 20.0), 0.0);
    }
}
