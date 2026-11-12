//! The probe: what turns a frame's embedding into probabilities. It was trained on the labeled frames (`tools/scene-context/scenelab/export.py`) and is mixed
//! with a zero-shot reading of the first 512 numbers (similarity of the whole frame to a sentence per class), which keeps it steady on
//! scenes the training frames did not cover.

use serde::Deserialize;

pub const STATES: usize = 4;

fn half() -> f64 {
    0.5
}

#[derive(Deserialize)]
struct Linear {
    mean: Vec<f64>,
    scale: Vec<f64>,
    coef: Vec<Vec<f64>>,
    intercept: Vec<f64>,
    classes: Vec<usize>,
}

#[derive(Deserialize)]
struct ZeroShot {
    temperature: f64,
    /// How much of the answer is the zero-shot reading; the probe has the rest.
    #[serde(default = "half")]
    weight: f64,
    /// on foot, land, air and water vehicle, menu-or-cutscene. They are as long as the first picture's embedding.
    text: Vec<Vec<f64>>,
}

#[derive(Deserialize)]
pub struct Probe {
    /// The pictures whose embeddings make up the input, in order (`encoder::layout`).
    #[serde(default)]
    layout: Vec<String>,
    context: Linear,
    view: Linear,
    zero_shot: ZeroShot,
}

fn softmax(z: &[f64]) -> Vec<f64> {
    let m = z.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let e: Vec<f64> = z.iter().map(|v| (v - m).exp()).collect();
    let s: f64 = e.iter().sum();
    e.into_iter().map(|v| v / s).collect()
}

impl Linear {
    fn check(&self, what: &str, dim: usize, outputs: usize) -> Result<(), String> {
        let rows = self.coef.len();
        // a two-class logistic regression stores a single row (the second class against the first)
        let rows_fit = if outputs == 2 { rows == 1 || rows == 2 } else { rows == self.classes.len() };
        let ok = self.mean.len() == dim
            && self.scale.len() == dim
            && self.scale.iter().all(|s| *s > 0.0)
            && self.coef.iter().all(|r| r.len() == dim)
            && rows == self.intercept.len()
            && rows_fit
            && self.classes.iter().all(|c| *c < outputs)
            && (outputs != 2 || self.classes == [0, 1]);
        if ok {
            Ok(())
        } else {
            Err(format!("the {what} probe does not fit a {dim}-number embedding"))
        }
    }

    fn logits(&self, e: &[f32]) -> Vec<f64> {
        self.coef
            .iter()
            .zip(&self.intercept)
            .map(|(row, b)| {
                row.iter().enumerate().map(|(i, w)| w * (f64::from(e[i]) - self.mean[i]) / self.scale[i]).sum::<f64>()
                    + b
            })
            .collect()
    }
}

impl Probe {
    pub fn from_json(text: &str) -> Result<Probe, String> {
        let p: Probe = serde_json::from_str(text).map_err(|e| format!("the scene probe is not readable: {e}"))?;
        let dim = p.context.mean.len();
        p.context.check("context", dim, STATES)?;
        p.view.check("view", dim, 2)?;
        let text = &p.zero_shot.text;
        if text.len() != 5
            || text.iter().any(|t| t.is_empty() || t.len() > dim || t.len() != text[0].len())
            || !(0.0..=1.0).contains(&p.zero_shot.weight)
        {
            return Err("the scene probe's zero-shot text does not fit".into());
        }
        Ok(p)
    }

    /// The length of the embedding this probe reads.
    pub fn dim(&self) -> usize {
        self.context.mean.len()
    }

    /// The pictures this probe's input is made of, in order (empty for a probe that does not say).
    pub fn layout(&self) -> &[String] {
        &self.layout
    }

    /// Probabilities of on foot, in vehicle, menu, cutscene for the joined embedding.
    pub fn context(&self, e: &[f32]) -> [f32; STATES] {
        assert_eq!(e.len(), self.dim());
        let mut probe = [0.0; STATES];
        for (p, c) in softmax(&self.context.logits(e)).into_iter().zip(&self.context.classes) {
            probe[*c] = p;
        }
        // the zero-shot reads the first picture's embedding, which starts the joined one
        let sims: Vec<f64> = self
            .zero_shot
            .text
            .iter()
            .map(|t| self.zero_shot.temperature * t.iter().zip(e).map(|(a, b)| a * f64::from(*b)).sum::<f64>())
            .collect();
        let z = softmax(&sims);
        // the sentences know "menu or cutscene" as one class: the probe splits it
        let share = probe[2] / (probe[2] + probe[3]).max(1e-9);
        let zero = [z[0], z[1] + z[2] + z[3], z[4] * share, z[4] * (1.0 - share)];
        let w = self.zero_shot.weight;
        let mut out = [0.0f32; STATES];
        for i in 0..STATES {
            out[i] = (w * zero[i] + (1.0 - w) * probe[i]) as f32;
        }
        out
    }

    /// Probabilities of first and third person.
    pub fn view(&self, e: &[f32]) -> [f32; 2] {
        assert_eq!(e.len(), self.dim());
        let l = self.view.logits(e);
        let third = if l.len() == 1 { 1.0 / (1.0 + (-l[0]).exp()) } else { softmax(&l)[1] };
        [(1.0 - third) as f32, third as f32]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tiny() -> String {
        // 2-number embeddings: class k likes direction k; the view probe likes the first number
        serde_json::json!({
            "context": {"mean": [0.0, 0.0], "scale": [1.0, 1.0], "classes": [0, 1, 2, 3],
                "coef": [[4.0, 0.0], [0.0, 4.0], [-4.0, 0.0], [0.0, -4.0]], "intercept": [0.0, 0.0, 0.0, 0.0]},
            "view": {"mean": [0.0, 0.0], "scale": [1.0, 1.0], "classes": [0, 1], "coef": [[-3.0, 0.0]], "intercept": [0.0]},
            "zero_shot": {"temperature": 10.0, "text": [[1.0, 0.0], [0.0, 1.0], [0.0, 1.0], [0.0, 1.0], [-1.0, 0.0]]}
        })
        .to_string()
    }

    #[test]
    fn probabilities_are_distributions_that_follow_the_embedding() {
        let p = Probe::from_json(&tiny()).unwrap();
        let on_foot = p.context(&[1.0, 0.0]);
        assert!((on_foot.iter().sum::<f32>() - 1.0).abs() < 1e-5);
        assert!(on_foot[0] > 0.9);
        let vehicle = p.context(&[0.0, 1.0]);
        assert!(vehicle[1] > 0.9);
        let menu_side = p.context(&[-1.0, 0.0]);
        assert!(menu_side[2] + menu_side[3] > 0.9);
        let v = p.view(&[1.0, 0.0]);
        assert!(v[0] > 0.9 && (v[0] + v[1] - 1.0).abs() < 1e-6);
        assert!(p.view(&[-1.0, 0.0])[1] > 0.9);
    }

    #[test]
    fn the_probe_splits_the_zero_shot_menu_mass_between_menu_and_cutscene() {
        let p = Probe::from_json(&tiny()).unwrap();
        let q = p.context(&[-1.0, 0.0]); // the probe prefers menu (class 2) here
        assert!(q[2] > q[3]);
    }

    #[test]
    fn the_zero_shot_reads_only_the_first_picture_and_has_the_weight_the_probe_says() {
        // 3 numbers: the first two are the whole frame (zero-shot reads those), the third only the probe sees
        let make = |weight: f64| {
            serde_json::json!({
                "layout": ["whole", "extra"],
                "context": {"mean": [0.0, 0.0, 0.0], "scale": [1.0, 1.0, 1.0], "classes": [0, 1, 2, 3],
                    "coef": [[0.0, 0.0, 4.0], [0.0, 0.0, -4.0], [0.0, 0.0, 0.0], [0.0, 0.0, 0.0]], "intercept": [0.0, 0.0, 0.0, 0.0]},
                "view": {"mean": [0.0, 0.0, 0.0], "scale": [1.0, 1.0, 1.0], "classes": [0, 1], "coef": [[0.0, 0.0, 1.0]], "intercept": [0.0]},
                "zero_shot": {"temperature": 10.0, "weight": weight, "text": [[1.0, 0.0], [0.0, 1.0], [0.0, 1.0], [0.0, 1.0], [-1.0, 0.0]]}
            })
            .to_string()
        };
        // the sentences say vehicle (second number), the probe says on foot (third number)
        let e = [0.0, 1.0, 1.0];
        let probe_only = Probe::from_json(&make(0.0)).unwrap();
        assert_eq!(probe_only.layout(), ["whole", "extra"]);
        assert_eq!(probe_only.dim(), 3);
        assert!(probe_only.context(&e)[0] > 0.9);
        assert!(Probe::from_json(&make(1.0)).unwrap().context(&e)[1] > 0.9);
        let mixed = Probe::from_json(&make(0.25)).unwrap().context(&e);
        assert!(mixed[0] > mixed[1], "the probe has the larger share: {mixed:?}");
        assert!(Probe::from_json(&make(1.5)).is_err());
    }

    #[test]
    fn a_probe_that_does_not_fit_is_refused() {
        assert!(Probe::from_json("{}").is_err());
        let bad = tiny().replace("[[1.0,0.0],[0.0,1.0]", "[[1.0],[0.0,1.0]");
        assert_ne!(bad, tiny());
        assert!(Probe::from_json(&bad).is_err());
        let zero_scale = tiny().replacen("\"scale\":[1.0,1.0]", "\"scale\":[0.0,1.0]", 1);
        assert!(Probe::from_json(&zero_scale).is_err());
    }
}
