//! Finds controller button prompts (the glyphs a game draws to tell you which button to press) in the stream's picture.
//!
//! Every candidate is a box that might hold a glyph. Two finders produce them: round things (a circular Hough vote on the edges, then
//! a refinement of centre and radius) and solid light shapes (a bright mask opened to drop thin lines, then connected components). A
//! candidate's box is stretched to a 24x24 patch and compared (zero-mean normalised correlation, either polarity) with templates of the
//! real PlayStation glyphs: Kenney's "Input Prompts" (CC0) for every button in its several drawing styles, plus a few glyphs sampled from
//! a live stream. Several shifted/scaled readings of the box are tried because box edges are only accurate to about a pixel.
//!
//! A [`Tracker`] then keeps only detections that persist over a few scans and smooths their position.
//! It runs on the CPU, on the half-size luma plane the renderer already makes, a few times a second, in its own thread.

/// A controller button or glyph the detector knows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Cross,
    Circle,
    Square,
    Triangle,
    L1,
    R1,
    L2,
    R2,
    LStick,
    RStick,
    L3,
    R3,
    DPad,
    Options,
    Create,
    Mute,
}

impl Kind {
    /// In the order the template file numbers them (see `scripts/make_prompt_glyphs.py`).
    pub const ALL: [Kind; 16] = [
        Kind::Cross,
        Kind::Circle,
        Kind::Square,
        Kind::Triangle,
        Kind::L1,
        Kind::R1,
        Kind::L2,
        Kind::R2,
        Kind::LStick,
        Kind::RStick,
        Kind::L3,
        Kind::R3,
        Kind::DPad,
        Kind::Options,
        Kind::Create,
        Kind::Mute,
    ];
    pub const FACE: [Kind; 4] = [Kind::Cross, Kind::Circle, Kind::Square, Kind::Triangle];

    /// The name the front end maps to keys: a control of the key map, or a stick or D-pad (which stands for four controls).
    pub fn control(self) -> &'static str {
        match self {
            Kind::Cross => "CROSS",
            Kind::Circle => "CIRCLE",
            Kind::Square => "SQUARE",
            Kind::Triangle => "TRIANGLE",
            Kind::L1 => "L1",
            Kind::R1 => "R1",
            Kind::L2 => "L2",
            Kind::R2 => "R2",
            Kind::LStick => "LEFT_STICK",
            Kind::RStick => "RIGHT_STICK",
            Kind::L3 => "L3",
            Kind::R3 => "R3",
            Kind::DPad => "DPAD",
            Kind::Options => "OPTIONS",
            Kind::Create => "CREATE",
            Kind::Mute => "MUTE",
        }
    }

    /// Glyphs that are recognised (so they are not mistaken for something else) but get no keycap.
    pub fn is_silent(self) -> bool {
        matches!(self, Kind::L3 | Kind::R3)
    }

    /// Kinds that look alike (they differ by a letter or a corner): telling them apart needs only a small margin.
    fn same_family(a: Kind, b: Kind) -> bool {
        use Kind::*;
        let fam = |k: Kind| match k {
            Circle | Square => 1,
            L1 | R1 => 2,
            L2 | R2 => 3,
            LStick | RStick => 4,
            L3 | R3 => 5,
            _ => 0,
        };
        fam(a) != 0 && fam(a) == fam(b)
    }
}

/// A detection in the luma plane's pixels (centre and size).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hit {
    pub kind: Kind,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub score: f32,
}

// ---- templates -----------------------------------------------------------------------------------------------------------------------

/// Side of the square patch a box is stretched to.
const GRID: usize = 24;

fn normalise(v: &mut [f32]) -> bool {
    let n = v.len() as f32;
    let mean = v.iter().sum::<f32>() / n;
    v.iter_mut().for_each(|x| *x -= mean);
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm < 1e-6 {
        return false;
    }
    v.iter_mut().for_each(|x| *x /= norm);
    true
}

struct Template {
    kind: Kind,
    /// Width / height of the glyph's box.
    aspect: f32,
    patch: Vec<f32>,
    /// The middle of the patch alone (the symbol or label, without the outline).
    inner: Vec<f32>,
}

/// The middle of a patch: where the symbol or label is, once the outline has been left out.
const INNER: std::ops::Range<usize> = 6..18;

fn inner_of(patch: &[f32]) -> Option<Vec<f32>> {
    let mut v: Vec<f32> = INNER.flat_map(|j| INNER.map(move |i| patch[j * GRID + i])).collect();
    normalise(&mut v).then_some(v)
}

/// Glyph templates: Kenney's art (`prompt_glyphs.bin`) and glyphs sampled from a live stream (`prompt_real.bin`), same format:
/// records of `kind: u8, aspect: f32 (little endian), GRID*GRID bytes`.
pub struct Templates {
    all: Vec<Template>,
}

const RECORD: usize = 1 + 4 + GRID * GRID;

fn parse_templates(bytes: &[u8], out: &mut Vec<Template>) {
    for r in bytes.as_chunks::<RECORD>().0 {
        let Some(&kind) = Kind::ALL.get(usize::from(r[0])) else { continue };
        let aspect = f32::from_le_bytes([r[1], r[2], r[3], r[4]]);
        let raw: Vec<f32> = r[5..].iter().map(|b| f32::from(*b)).collect();
        let inner = inner_of(&raw);
        let mut patch = raw;
        if let (true, Some(inner)) = (normalise(&mut patch), inner) {
            out.push(Template { kind, aspect, patch, inner });
        }
    }
}

impl Templates {
    pub fn new() -> Self {
        let mut all = vec![];
        parse_templates(include_bytes!("prompt_glyphs.bin"), &mut all);
        parse_templates(include_bytes!("prompt_real.bin"), &mut all);
        Self { all }
    }

    pub fn len(&self) -> usize {
        self.all.len()
    }

    pub fn is_empty(&self) -> bool {
        self.all.is_empty()
    }
}

impl Default for Templates {
    fn default() -> Self {
        Self::new()
    }
}

// ---- the image side -------------------------------------------------------------------------------------------------------------------

/// A luma plane (0..255).
pub struct Plane<'a> {
    pub data: &'a [u8],
    pub w: usize,
    pub h: usize,
}

impl Plane<'_> {
    fn at(&self, x: isize, y: isize) -> f32 {
        f32::from(
            self.data[(y.clamp(0, self.h as isize - 1) as usize) * self.w + x.clamp(0, self.w as isize - 1) as usize],
        )
    }

    fn bilinear(&self, x: f32, y: f32) -> f32 {
        let (x0, y0) = (x.floor(), y.floor());
        let (tx, ty) = (x - x0, y - y0);
        let (x0, y0) = (x0 as isize, y0 as isize);
        let a = self.at(x0, y0) * (1.0 - tx) + self.at(x0 + 1, y0) * tx;
        let b = self.at(x0, y0 + 1) * (1.0 - tx) + self.at(x0 + 1, y0 + 1) * tx;
        a * (1.0 - ty) + b * ty
    }
}

/// Smallest and largest round glyph radius searched, in luma pixels.
pub const R_MIN: usize = 4;
pub const R_MAX: usize = 36;
/// Edge strength (Sobel magnitude, 0..~1000) above which a pixel votes.
const EDGE_MIN: f32 = 90.0;
/// Minimum share of a circle's circumference that must vote for it.
const VOTE_SHARE: f32 = 0.5;
/// Minimum template correlation, and the margin over the best other kind (smaller for kinds that look alike).
pub const MIN_SCORE: f32 = 0.75;
/// Stick icons and L3/R3 are a small letter on a disc, which round letters of ordinary text and round game art resemble: they need a
/// closer match than the other glyphs.
pub const MIN_SCORE_DISC_LETTER: f32 = 0.9;

fn min_score(kind: Kind) -> f32 {
    if matches!(kind, Kind::LStick | Kind::RStick | Kind::L3 | Kind::R3) {
        MIN_SCORE_DISC_LETTER
    } else {
        MIN_SCORE
    }
}
pub const MIN_MARGIN: f32 = 0.06;
pub const MIN_MARGIN_FAMILY: f32 = 0.02;

struct Edges {
    ux: Vec<f32>,
    uy: Vec<f32>,
    mag: Vec<f32>,
    /// Edges weaker than this do not vote: `EDGE_MIN`, or higher in a busy picture so the work stays bounded.
    threshold: f32,
}

/// At most this share of the pixels may vote. Prompts are strong, isolated edges, so a busy picture loses only its faint detail.
const MAX_VOTING_SHARE: f32 = 0.05;
/// Luma above which a pixel can belong to a light glyph.
const FILL_MIN: u8 = 140;

fn gradients(p: &Plane) -> Edges {
    let n = p.w * p.h;
    let (mut ux, mut uy, mut mag) = (vec![0.0f32; n], vec![0.0f32; n], vec![0.0f32; n]);
    for y in 0..p.h as isize {
        for x in 0..p.w as isize {
            let gx = -p.at(x - 1, y - 1) - 2.0 * p.at(x - 1, y) - p.at(x - 1, y + 1)
                + p.at(x + 1, y - 1)
                + 2.0 * p.at(x + 1, y)
                + p.at(x + 1, y + 1);
            let gy = -p.at(x - 1, y - 1) - 2.0 * p.at(x, y - 1) - p.at(x + 1, y - 1)
                + p.at(x - 1, y + 1)
                + 2.0 * p.at(x, y + 1)
                + p.at(x + 1, y + 1);
            let m = (gx * gx + gy * gy).sqrt();
            let i = y as usize * p.w + x as usize;
            mag[i] = m;
            if m > 1e-3 {
                ux[i] = gx / m;
                uy[i] = gy / m;
            }
        }
    }
    // adaptive threshold: the weakest magnitude that keeps the voters under the cap
    let mut threshold = EDGE_MIN;
    let strong = mag.iter().filter(|m| **m >= EDGE_MIN).count();
    let cap = (n as f32 * MAX_VOTING_SHARE) as usize;
    if strong > cap {
        let mut hist = [0usize; 128];
        for m in &mag {
            hist[((*m / 8.0) as usize).min(127)] += 1;
        }
        let (mut acc, mut bin) = (0usize, 127usize);
        while bin > 0 && acc + hist[bin] <= cap {
            acc += hist[bin];
            bin -= 1;
        }
        threshold = threshold.max((bin + 1) as f32 * 8.0);
    }
    Edges { ux, uy, mag, threshold }
}

/// Round badges: `(centre x, centre y, radius, votes / circumference)`.
fn circles(p: &Plane, e: &Edges) -> Vec<(f32, f32, usize, f32)> {
    // the accumulator is half the plane's size: a centre need only be found to within a pixel or two
    let (aw, ah) = (p.w.div_ceil(2), p.h.div_ceil(2));
    let nr = R_MAX - R_MIN + 1;
    let mut acc = vec![0u16; aw * ah * nr];
    for y in 0..p.h {
        for x in 0..p.w {
            let i = y * p.w + x;
            if e.mag[i] < e.threshold {
                continue;
            }
            let (ux, uy) = (e.ux[i], e.uy[i]);
            // thinning: only the ridge of an edge votes (a soft edge is several pixels wide and would vote several times)
            let (fx, fy) = ((x as f32 + ux).round(), (y as f32 + uy).round());
            let (bx, by) = ((x as f32 - ux).round(), (y as f32 - uy).round());
            let mag_at = |px: f32, py: f32| -> f32 {
                if px < 0.0 || py < 0.0 || px >= p.w as f32 || py >= p.h as f32 {
                    0.0
                } else {
                    e.mag[py as usize * p.w + px as usize]
                }
            };
            if e.mag[i] < mag_at(fx, fy) || e.mag[i] <= mag_at(bx, by) {
                continue;
            }
            for sign in [-1.0f32, 1.0] {
                for (ri, r) in (R_MIN..=R_MAX).enumerate() {
                    let cx = x as f32 + sign * ux * r as f32;
                    let cy = y as f32 + sign * uy * r as f32;
                    if cx < 0.0 || cy < 0.0 {
                        continue;
                    }
                    let (ax, ay) = ((cx * 0.5) as usize, (cy * 0.5) as usize);
                    if ax < aw && ay < ah {
                        let a = &mut acc[(ri * ah + ay) * aw + ax];
                        *a = a.saturating_add(1);
                    }
                }
            }
        }
    }
    // Each vote lands in one cell, but gradient-direction noise spreads a rim's votes over neighbouring cells and radii (more the larger
    // the radius): score a cell by the vote mass of its 3x3x3 neighbourhood, then keep local maxima of that score.
    let at = |ri: isize, ay: isize, ax: isize| -> f32 {
        if ri < 0 || ri >= nr as isize || ay < 0 || ay >= ah as isize || ax < 0 || ax >= aw as isize {
            0.0
        } else {
            f32::from(acc[(ri as usize * ah + ay as usize) * aw + ax as usize])
        }
    };
    let mass = |ri: usize, ay: usize, ax: usize| -> f32 {
        let mut m = 0.0;
        for dr in -1isize..=1 {
            for dy in -1isize..=1 {
                for dx in -1isize..=1 {
                    m += at(ri as isize + dr, ay as isize + dy, ax as isize + dx);
                }
            }
        }
        m
    };
    let mut found = vec![];
    for ri in 0..nr {
        let r = R_MIN + ri;
        let circumference = std::f32::consts::TAU * r as f32;
        for ay in 1..ah.saturating_sub(1) {
            for ax in 1..aw.saturating_sub(1) {
                // cheap pre-filter on the raw cell, then the neighbourhood mass
                if at(ri as isize, ay as isize, ax as isize) < 0.10 * circumference {
                    continue;
                }
                let m = mass(ri, ay, ax);
                if m < VOTE_SHARE * circumference {
                    continue;
                }
                let mut best = true;
                'n: for dr in -1isize..=1 {
                    for dy in -1isize..=1 {
                        for dx in -1isize..=1 {
                            if dr == 0 && dx == 0 && dy == 0 {
                                continue;
                            }
                            let (rj, yj, xj) = (ri as isize + dr, ay as isize + dy, ax as isize + dx);
                            if rj < 0
                                || rj >= nr as isize
                                || yj < 1
                                || yj >= ah as isize - 1
                                || xj < 1
                                || xj >= aw as isize - 1
                            {
                                continue;
                            }
                            let w = mass(rj as usize, yj as usize, xj as usize);
                            if w > m || (w == m && (dr, dy, dx) < (0, 0, 0)) {
                                best = false;
                                break 'n;
                            }
                        }
                    }
                }
                if best {
                    found.push((ax as f32 * 2.0 + 1.0, ay as f32 * 2.0 + 1.0, r, m / circumference));
                }
            }
        }
    }
    found
}

/// The round badges alone (before the symbol is read): `(x, y, radius, votes / circumference)`. For tests and tuning.
pub fn circle_candidates(p: &Plane) -> Vec<(f32, f32, usize, f32)> {
    circles(p, &gradients(p))
}

/// Refine a Hough circle: slide the centre by up to 2 pixels and the radius by up to 2 and keep the placement whose rim has the strongest
/// edges pointing along the radius.
fn refine(p: &Plane, e: &Edges, cx: f32, cy: f32, r: usize) -> (f32, f32, f32) {
    let mut best = (cx, cy, r as f32, -1.0f32);
    for dy in -2..=2 {
        for dx in -2..=2 {
            for dr in -2..=2 {
                let (x0, y0, rr) = (cx + dx as f32, cy + dy as f32, r as f32 + dr as f32);
                if rr < 3.0 {
                    continue;
                }
                let mut sum = 0.0;
                let steps = 24;
                for k in 0..steps {
                    let a = k as f32 / steps as f32 * std::f32::consts::TAU;
                    let (px, py) = (x0 + rr * a.cos(), y0 + rr * a.sin());
                    let (ix, iy) = (px.round() as isize, py.round() as isize);
                    if ix < 0 || iy < 0 || ix >= p.w as isize || iy >= p.h as isize {
                        continue;
                    }
                    let i = iy as usize * p.w + ix as usize;
                    sum += e.mag[i] * (e.ux[i] * a.cos() + e.uy[i] * a.sin()).abs();
                }
                if sum > best.3 {
                    best = (x0, y0, rr, sum);
                }
            }
        }
    }
    (best.0, best.1, best.2)
}

/// Solid light shapes the size and proportions of a shoulder-button glyph (a pill or a shield): `(x, y, w, h)` of each. A bright mask is
/// opened with a 3x3 block, which removes thin lines and speckle and keeps filled shapes; pieces the opening split (the thin top and
/// bottom of a pill) are joined again before the box is measured.
pub fn filled_candidates(p: &Plane) -> Vec<(f32, f32, f32, f32)> {
    let (w, h) = (p.w, p.h);
    if w < 16 || h < 16 {
        return vec![];
    }
    let bright: Vec<bool> = p.data[..w * h].iter().map(|v| *v >= FILL_MIN).collect();
    // erode with a 3x3 block: a pixel survives if it and its eight neighbours are bright
    let mut tmp = vec![false; w * h];
    for y in 0..h {
        for x in 1..w - 1 {
            tmp[y * w + x] = bright[y * w + x - 1] && bright[y * w + x] && bright[y * w + x + 1];
        }
    }
    let mut core = vec![false; w * h];
    for y in 1..h - 1 {
        for x in 0..w {
            core[y * w + x] = tmp[(y - 1) * w + x] && tmp[y * w + x] && tmp[(y + 1) * w + x];
        }
    }
    let dilate = |src: &[bool]| -> Vec<bool> {
        let mut t = vec![false; w * h];
        for y in 0..h {
            for x in 0..w {
                t[y * w + x] = src[y * w + x] || (x > 0 && src[y * w + x - 1]) || (x + 1 < w && src[y * w + x + 1]);
            }
        }
        let mut o = vec![false; w * h];
        for y in 0..h {
            for x in 0..w {
                o[y * w + x] = t[y * w + x] || (y > 0 && t[(y - 1) * w + x]) || (y + 1 < h && t[(y + 1) * w + x]);
            }
        }
        o
    };
    // grow the cores back to the shape (3x3), limited to the bright mask
    let grown: Vec<bool> = dilate(&core).iter().zip(&bright).map(|(g, b)| *g && *b).collect();
    // join pieces that the opening split: label on a mask dilated by two more pixels
    let joined = dilate(&dilate(&grown));
    let mut seen = vec![false; w * h];
    let mut out = vec![];
    let mut stack = vec![];
    for sy in 0..h {
        for sx in 0..w {
            if !joined[sy * w + sx] || seen[sy * w + sx] {
                continue;
            }
            let (mut x0, mut x1, mut y0, mut y1, mut area) = (usize::MAX, 0usize, usize::MAX, 0usize, 0usize);
            let mut pixels = vec![];
            stack.push((sx, sy));
            seen[sy * w + sx] = true;
            while let Some((x, y)) = stack.pop() {
                if grown[y * w + x] {
                    area += 1;
                    pixels.push((x, y));
                    x0 = x0.min(x);
                    x1 = x1.max(x);
                    y0 = y0.min(y);
                    y1 = y1.max(y);
                }
                for (nx, ny) in [(x.wrapping_sub(1), y), (x + 1, y), (x, y.wrapping_sub(1)), (x, y + 1)] {
                    if nx < w && ny < h && joined[ny * w + nx] && !seen[ny * w + nx] {
                        seen[ny * w + nx] = true;
                        stack.push((nx, ny));
                    }
                }
            }
            if pixels.is_empty() {
                continue;
            }
            // grow the box back over the bright pixels within two pixels of what is left (the opening ate the corners)
            let (gx0, gx1, gy0, gy1) =
                (x0.saturating_sub(2), (x1 + 2).min(w - 1), y0.saturating_sub(2), (y1 + 2).min(h - 1));
            let fw = gx1 - gx0 + 1;
            let mut full = vec![false; fw * (gy1 - gy0 + 1)];
            for &(x, y) in &pixels {
                full[(y - gy0) * fw + (x - gx0)] = true;
            }
            let mut st = pixels;
            while let Some((x, y)) = st.pop() {
                x0 = x0.min(x);
                x1 = x1.max(x);
                y0 = y0.min(y);
                y1 = y1.max(y);
                for (nx, ny) in [(x.wrapping_sub(1), y), (x + 1, y), (x, y.wrapping_sub(1)), (x, y + 1)] {
                    if nx >= gx0
                        && nx <= gx1
                        && ny >= gy0
                        && ny <= gy1
                        && bright[ny * w + nx]
                        && !full[(ny - gy0) * fw + (nx - gx0)]
                    {
                        full[(ny - gy0) * fw + (nx - gx0)] = true;
                        st.push((nx, ny));
                    }
                }
            }
            let (bw, bh) = ((x1 - x0 + 1) as f32, (y1 - y0 + 1) as f32);
            if (6.0..=110.0).contains(&bw)
                && (6.0..=60.0).contains(&bh)
                && (0.35..=2.8).contains(&(bw / bh))
                && area as f32 >= bw * bh * 0.35
            {
                out.push((x0 as f32 + bw / 2.0, y0 as f32 + bh / 2.0, bw, bh));
            }
        }
    }
    out
}

/// Stretch the box of size `bw x bh` centred at `(cx, cy)` to a `GRID x GRID` patch.
pub fn box_patch(p: &Plane, cx: f32, cy: f32, bw: f32, bh: f32) -> Vec<f32> {
    let (x0, y0) = (cx - bw / 2.0, cy - bh / 2.0);
    let mut patch = vec![0.0f32; GRID * GRID];
    for j in 0..GRID {
        for i in 0..GRID {
            patch[j * GRID + i] = p.bilinear(
                x0 + (i as f32 + 0.5) / GRID as f32 * bw - 0.5,
                y0 + (j as f32 + 0.5) / GRID as f32 * bh - 0.5,
            );
        }
    }
    patch
}

/// How many of 16 directions just outside a box show the same tone as the inside of the glyph (a neighbouring letter of the same text,
/// a line of the map): a prompt glyph stands alone on its background, so for it this is near zero.
pub fn rim_touching(p: &Plane, cx: f32, cy: f32, bw: f32, bh: f32) -> usize {
    let at = |a: f32, k: f32| p.bilinear(cx + k * 0.5 * bw * a.cos(), cy + k * 0.5 * bh * a.sin());
    let angle = |i: usize| i as f32 / 16.0 * std::f32::consts::TAU;
    let inside: f32 = (0..16).map(|i| at(angle(i), 0.7)).sum::<f32>() / 16.0;
    let outs: Vec<f32> = (0..16).map(|i| at(angle(i), 1.4)).collect();
    let (lo, hi) = outs.iter().fold((inside, inside), |(l, h), v| (l.min(*v), h.max(*v)));
    let range = (hi - lo).max(40.0);
    outs.iter().filter(|v| (**v - inside).abs() < 0.25 * range).count()
}

/// Boxes with this many "touching" directions are part of something bigger (text, map lines), not a prompt glyph.
const MAX_TOUCHING: usize = 0;
/// The smallest glyph worth reading: below this there is nothing left to tell glyphs from specks.
const MIN_GLYPH: f32 = 9.0;

/// What a box reads as: `(kind, score, margin)`, or `None` when it is too flat or nothing is close.
pub fn classify_box(p: &Plane, tpl: &Templates, cx: f32, cy: f32, bw: f32, bh: f32) -> Option<(Kind, f32, f32)> {
    if bw.max(bh) < MIN_GLYPH || bw.min(bh) < 5.0 || rim_touching(p, cx, cy, bw, bh) > MAX_TOUCHING {
        return None;
    }
    let aspect = bw / bh;
    // the box edge is only good to about a pixel: read the box a little smaller and a little larger as well
    let mut best: Vec<(Kind, f32)> = vec![];
    let mut any = false;
    for scale in [1.0f32, 0.9, 1.1] {
        let raw = box_patch(p, cx, cy, bw * scale, bh * scale);
        let (lo, hi) = raw.iter().fold((f32::MAX, f32::MIN), |(l, h), v| (l.min(*v), h.max(*v)));
        let Some(inner) = inner_of(&raw) else { continue };
        let mut patch = raw;
        if hi - lo < 24.0 || !normalise(&mut patch) {
            continue;
        }
        for t in &tpl.all {
            let r = aspect / t.aspect;
            if !(0.7..=1.43).contains(&r) {
                continue;
            }
            any = true;
            // the outline decides the family, the middle decides which member: both count
            let whole = patch.iter().zip(&t.patch).map(|(a, b)| a * b).sum::<f32>().abs();
            let mid = inner.iter().zip(&t.inner).map(|(a, b)| a * b).sum::<f32>().abs();
            let c = 0.5 * (whole + mid);
            match best.iter_mut().find(|b| b.0 == t.kind) {
                Some(b) => b.1 = b.1.max(c),
                None => best.push((t.kind, c)),
            }
        }
    }
    if !any || best.is_empty() {
        return None;
    }
    best.sort_by(|a, b| b.1.total_cmp(&a.1));
    let (first, second) = (best[0], best.get(1).copied());
    let margin = match second {
        // a kind that looks like another one is allowed a smaller lead, so that it is not thrown out for it
        Some(s) if Kind::same_family(first.0, s.0) => first.1 - s.1 + (MIN_MARGIN - MIN_MARGIN_FAMILY),
        Some(s) => first.1 - s.1,
        None => first.1,
    };
    Some((first.0, first.1, margin))
}

/// How well the box at `(cx, cy)` still looks like a glyph of `kind`: the best correlation over that kind's templates, trying the box a pixel
/// to each side and a little smaller and larger. This is the cheap per-frame check of a glyph that was already found; it also returns where
/// the box fits best.
pub fn verify_kind(
    p: &Plane,
    tpl: &Templates,
    kind: Kind,
    cx: f32,
    cy: f32,
    bw: f32,
    bh: f32,
) -> Option<(f32, f32, f32)> {
    let aspect = bw / bh.max(1.0);
    let mut best: Option<(f32, f32, f32)> = None;
    for (dx, dy) in [(0.0, 0.0), (1.0, 0.0), (-1.0, 0.0), (0.0, 1.0), (0.0, -1.0)] {
        for scale in [1.0f32, 0.92, 1.08] {
            let raw = box_patch(p, cx + dx, cy + dy, bw * scale, bh * scale);
            let (lo, hi) = raw.iter().fold((f32::MAX, f32::MIN), |(l, h), v| (l.min(*v), h.max(*v)));
            let Some(inner) = inner_of(&raw) else { continue };
            let mut patch = raw;
            if hi - lo < 16.0 || !normalise(&mut patch) {
                continue;
            }
            for t in tpl.all.iter().filter(|t| t.kind == kind && (0.65..=1.55).contains(&(aspect / t.aspect))) {
                let whole = patch.iter().zip(&t.patch).map(|(a, b)| a * b).sum::<f32>().abs();
                let mid = inner.iter().zip(&t.inner).map(|(a, b)| a * b).sum::<f32>().abs();
                let c = 0.5 * (whole + mid);
                if best.is_none_or(|b| c > b.0) {
                    best = Some((c, cx + dx, cy + dy));
                }
            }
        }
    }
    best
}

fn accept(c: Option<(Kind, f32, f32)>) -> Option<(Kind, f32)> {
    let (kind, score, margin) = c?;
    (score >= min_score(kind) && margin >= MIN_MARGIN).then_some((kind, score))
}

/// A round candidate: centre x, centre y, radius, and how it was read (`(kind, score, margin)`).
pub type BadgeCandidate = (f32, f32, f32, Option<(Kind, f32, f32)>);

/// Every round candidate with how it was read, accepted or not.
pub fn badge_candidates(p: &Plane, tpl: &Templates) -> Vec<BadgeCandidate> {
    let e = gradients(p);
    circles(p, &e)
        .into_iter()
        .map(|(cx, cy, r, _share)| {
            let (cx, cy, rr) = refine(p, &e, cx, cy, r);
            (cx, cy, rr, classify_box(p, tpl, cx, cy, rr * 2.0, rr * 2.0))
        })
        .collect()
}

fn round_hits(p: &Plane, tpl: &Templates) -> Vec<Hit> {
    badge_candidates(p, tpl)
        .into_iter()
        .filter_map(|(cx, cy, rr, c)| {
            accept(c).map(|(kind, score)| Hit { kind, x: cx, y: cy, w: rr * 2.0, h: rr * 2.0, score })
        })
        .collect()
}

fn solid_hits(p: &Plane, tpl: &Templates) -> Vec<Hit> {
    filled_candidates(p)
        .into_iter()
        .filter_map(|(x, y, w, h)| {
            // the box is measured on pixels bright enough to count as fill; the glyph's outline is about a pixel further out
            accept(classify_box(p, tpl, x, y, w + 1.0, h + 1.0)).map(|(kind, score)| Hit { kind, x, y, w, h, score })
        })
        .collect()
}

/// A luma plane shrunk (bilinear, with a 2x2 box first when the shrink is large) so its width is about `target_w`; unchanged if smaller.
pub fn shrink(data: &[u8], w: usize, h: usize, target_w: usize) -> (Vec<u8>, usize, usize) {
    if w <= target_w + target_w / 8 || w == 0 || h == 0 || data.len() < w * h {
        return (data.to_vec(), w, h);
    }
    let (ow, oh) = (target_w, (h * target_w / w).max(1));
    let plane = Plane { data, w, h };
    let (sx, sy) = (w as f32 / ow as f32, h as f32 / oh as f32);
    let mut out = vec![0u8; ow * oh];
    for y in 0..oh {
        for x in 0..ow {
            // average of four taps spread over the source footprint of this output pixel
            let mut sum = 0.0;
            for (dx, dy) in [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)] {
                sum += plane.bilinear((x as f32 + dx) * sx - 0.5, (y as f32 + dy) * sy - 0.5);
            }
            out[y * ow + x] = (sum / 4.0).round().clamp(0.0, 255.0) as u8;
        }
    }
    (out, ow, oh)
}

/// Everything in one luma plane, best hits first.
pub fn detect(p: &Plane, tpl: &Templates) -> Vec<Hit> {
    if p.w < 32 || p.h < 32 || p.data.len() < p.w * p.h {
        return vec![];
    }
    let mut hits = round_hits(p, tpl);
    hits.extend(solid_hits(p, tpl));
    non_max(hits)
}

/// Keep the best hit among overlapping ones.
pub fn non_max(mut hits: Vec<Hit>) -> Vec<Hit> {
    hits.sort_by(|a, b| b.score.total_cmp(&a.score));
    let mut kept: Vec<Hit> = vec![];
    for h in hits {
        let clash =
            kept.iter().any(|k| (k.x - h.x).abs() < (k.w + h.w) * 0.35 && (k.y - h.y).abs() < (k.h + h.h) * 0.35);
        if !clash {
            kept.push(h);
        }
    }
    kept
}

// ---- the scene moves, the HUD does not -------------------------------------------------------------------------------------------------

/// Per-pixel stability of the picture over the last few frames. A prompt drawn over gameplay is part of the HUD: it stays where it is while
/// the 3D scene behind it moves. Pixels that keep changing are scene; the ones that do not are candidates for HUD.
pub struct Stability {
    prev: Vec<u8>,
    /// Smoothed absolute change per pixel, times 16.
    change: Vec<u16>,
    size: (usize, usize),
    frames: u32,
}

/// A pixel whose smoothed change is above this (in luma levels) is scene, not HUD. A translucent prompt over a moving scene still shows 15 % of
/// the scene's change, so this is not set too low.
const MOVING_LEVEL: u16 = 10 * 16;
/// Frames before the stability map is trusted.
const WARM_FRAMES: u32 = 8;
/// Share of moving pixels above which the picture counts as a moving scene.
const MOVING_SHARE: f32 = 0.2;

impl Default for Stability {
    fn default() -> Self {
        Self::new()
    }
}

impl Stability {
    pub fn new() -> Self {
        Self { prev: vec![], change: vec![], size: (0, 0), frames: 0 }
    }

    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// Feed the next frame.
    pub fn update(&mut self, p: &Plane) {
        let n = p.w * p.h;
        if self.size != (p.w, p.h) || p.data.len() < n {
            self.size = (p.w, p.h);
            self.prev = p.data[..n.min(p.data.len())].to_vec();
            self.change = vec![0; self.prev.len()];
            self.frames = 0;
            return;
        }
        for ((c, prev), cur) in self.change.iter_mut().zip(self.prev.iter_mut()).zip(&p.data[..n]) {
            let d = (i32::from(*cur) - i32::from(*prev)).unsigned_abs() as i32 * 16;
            *c = (i32::from(*c) + (d - i32::from(*c)) / 4) as u16;
            *prev = *cur;
        }
        self.frames = self.frames.saturating_add(1);
    }

    /// Is this a moving scene (enough pixels keep changing), once the map has warmed up?
    pub fn scene_is_moving(&self) -> bool {
        if self.frames < WARM_FRAMES || self.change.is_empty() {
            return false;
        }
        let moving = self.change.iter().filter(|c| **c > MOVING_LEVEL).count();
        moving as f32 / self.change.len() as f32 > MOVING_SHARE
    }

    /// The picture with the moving scene blanked to `fill` (a pixel stays if it or a neighbour is stable): what is left is the HUD.
    pub fn hud_only(&self, p: &Plane, fill: u8) -> Vec<u8> {
        let (w, h) = (p.w, p.h);
        let stable: Vec<bool> = self.change.iter().map(|c| *c <= MOVING_LEVEL).collect();
        let mut out = vec![fill; w * h];
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                let keep = stable[i]
                    || (x > 0 && stable[i - 1])
                    || (x + 1 < w && stable[i + 1])
                    || (y > 0 && stable[i - w])
                    || (y + 1 < h && stable[i + w]);
                if keep {
                    out[i] = p.data[i];
                }
            }
        }
        out
    }
}

/// Everything in a picture, using what the last frames showed: over a moving scene the picture is also searched with the scene blanked
/// (once to black, once to white, so a light or a dark glyph stands alone), which is how a prompt drawn over gameplay is found.
pub fn detect_with_history(p: &Plane, tpl: &Templates, st: &Stability) -> Vec<Hit> {
    let mut hits = detect(p, tpl);
    if st.scene_is_moving() && st.size == (p.w, p.h) {
        for fill in [0u8, 255u8] {
            let d = st.hud_only(p, fill);
            hits.extend(detect(&Plane { data: &d, w: p.w, h: p.h }, tpl));
        }
        hits = non_max(hits);
    }
    hits
}

// ---- tracking -----------------------------------------------------------------------------------------------------------------------

/// A glyph being followed, in 0..1 coordinates of the picture. `id` stays the same for as long as the glyph is followed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Detection {
    pub id: u64,
    pub kind: Kind,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub score: f32,
}

struct Track {
    id: u64,
    kind: Kind,
    /// Centre and size in plane pixels.
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    score: f32,
    shown: bool,
    /// Full scans that found it / that did not (in a row).
    seen: u32,
    missed: u32,
    /// Quick checks that failed in a row.
    fails: u32,
}

/// A hit this strong is shown at once; a weaker one needs a second sighting.
const SHOW_AT: f32 = 0.88;
/// A shown glyph is dropped after this many quick checks fail in a row (the checks run on every frame that arrives).
const DROP_AFTER_FAILS: u32 = 2;
/// A quick check passes at this share of the kind's score floor.
const VERIFY_SHARE: f32 = 0.78;
/// A glyph that no full scan has found for this many scans is dropped even if the quick checks still pass.
const DROP_AFTER_MISSED_SCANS: u32 = 8;

/// Follows glyphs between frames. Full scans find them (slow); a quick check of each followed glyph's own box runs on every frame that
/// arrives (fast), so a glyph is shown as soon as one scan is sure of it and gone a couple of frames after it is.
pub struct Tracker {
    tracks: Vec<Track>,
    plane: (usize, usize),
    next_id: u64,
}

impl Default for Tracker {
    fn default() -> Self {
        Self::new()
    }
}

impl Tracker {
    pub fn new() -> Self {
        Self { tracks: vec![], plane: (1, 1), next_id: 1 }
    }

    /// A full scan's hits (plane pixels). Returns true if what is shown changed.
    pub fn discovery(&mut self, hits: &[Hit], plane_w: usize, plane_h: usize) -> bool {
        self.plane = (plane_w.max(1), plane_h.max(1));
        let before = self.snapshot();
        let mut used = vec![false; hits.len()];
        for t in &mut self.tracks {
            let found = hits.iter().enumerate().find(|(i, h)| {
                !used[*i]
                    && h.kind == t.kind
                    && (h.x - t.x).abs() < h.w.max(8.0) * 0.6
                    && (h.y - t.y).abs() < h.h.max(8.0) * 0.6
            });
            if let Some((i, h)) = found {
                used[i] = true;
                t.x += (h.x - t.x) * 0.5;
                t.y += (h.y - t.y) * 0.5;
                t.w += (h.w - t.w) * 0.5;
                t.h += (h.h - t.h) * 0.5;
                t.score = h.score;
                t.seen += 1;
                t.missed = 0;
                t.fails = 0;
                t.shown = t.shown || t.seen >= 2 || h.score >= SHOW_AT;
            } else {
                t.missed += 1;
            }
        }
        for (i, h) in hits.iter().enumerate() {
            if !used[i] {
                self.tracks.push(Track {
                    id: self.next_id,
                    kind: h.kind,
                    x: h.x,
                    y: h.y,
                    w: h.w,
                    h: h.h,
                    score: h.score,
                    shown: h.score >= SHOW_AT,
                    seen: 1,
                    missed: 0,
                    fails: 0,
                });
                self.next_id += 1;
            }
        }
        // one that was never confirmed and was not seen again is noise; a shown one stays until the quick checks fail
        self.tracks.retain(|t| if t.shown { t.missed < DROP_AFTER_MISSED_SCANS } else { t.missed < 2 });
        before != self.snapshot()
    }

    /// The quick check of every followed glyph on a new frame. Returns true if what is shown changed.
    pub fn verify(&mut self, p: &Plane, tpl: &Templates) -> bool {
        if self.tracks.is_empty() {
            return false;
        }
        let before = self.snapshot();
        for t in &mut self.tracks {
            let ok = match verify_kind(p, tpl, t.kind, t.x, t.y, t.w + 1.0, t.h + 1.0) {
                Some((score, x, y)) if score >= min_score(t.kind) * VERIFY_SHARE => {
                    // follow a glyph that drifts by a pixel (it is a pulsing or moving prompt, not a new one)
                    t.x = x;
                    t.y = y;
                    true
                }
                _ => false,
            };
            t.fails = if ok { 0 } else { t.fails + 1 };
        }
        self.tracks.retain(|t| t.fails < DROP_AFTER_FAILS);
        before != self.snapshot()
    }

    /// What is shown now, in 0..1 coordinates.
    pub fn detections(&self) -> Vec<Detection> {
        let (iw, ih) = (self.plane.0 as f32, self.plane.1 as f32);
        self.tracks
            .iter()
            .filter(|t| t.shown)
            .map(|t| Detection {
                id: t.id,
                kind: t.kind,
                x: t.x / iw,
                y: t.y / ih,
                w: t.w / iw,
                h: t.h / ih,
                score: t.score,
            })
            .collect()
    }

    /// What a change would be visible as: the shown glyphs with positions to the nearest pixel.
    fn snapshot(&self) -> Vec<(u64, i32, i32)> {
        self.tracks.iter().filter(|t| t.shown).map(|t| (t.id, t.x.round() as i32, t.y.round() as i32)).collect()
    }

    pub fn clear(&mut self) {
        self.tracks.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shrinking_keeps_the_picture_and_leaves_small_ones_alone() {
        let (w, h) = (1000, 500);
        let src: Vec<u8> = (0..w * h).map(|i| ((i % w) * 255 / w) as u8).collect();
        let (out, ow, oh) = shrink(&src, w, h, 500);
        assert_eq!((ow, oh), (500, 250));
        assert!(out[250] > 120 && out[250] < 135, "a left-to-right ramp stays a ramp: {}", out[250]);
        let (same, sw, sh) = shrink(&src, w, h, 1000);
        assert_eq!((sw, sh, same.len()), (w, h, src.len()));
        assert_eq!(shrink(&[], 0, 0, 640).1, 0);
    }

    #[test]
    fn every_kind_has_templates_and_all_of_them_are_normalised() {
        let t = Templates::new();
        assert!(!t.is_empty());
        for k in Kind::ALL {
            assert!(t.all.iter().any(|x| x.kind == k), "{k:?} has no template");
        }
        for x in &t.all {
            assert!((x.patch.iter().map(|v| v * v).sum::<f32>() - 1.0).abs() < 1e-3);
            assert!((x.inner.iter().map(|v| v * v).sum::<f32>() - 1.0).abs() < 1e-3);
            assert!(x.aspect > 0.2 && x.aspect < 5.0, "{:?}: {}", x.kind, x.aspect);
        }
    }

    #[test]
    fn kinds_map_to_distinct_control_names_and_only_l3_r3_are_silent() {
        let names: std::collections::HashSet<_> = Kind::ALL.iter().map(|k| k.control()).collect();
        assert_eq!(names.len(), Kind::ALL.len());
        let silent: Vec<_> = Kind::ALL.iter().filter(|k| k.is_silent()).collect();
        assert_eq!(silent, [&Kind::L3, &Kind::R3]);
    }

    fn hit(kind: Kind, x: f32, score: f32) -> Hit {
        Hit { kind, x, y: 80.0, w: 30.0, h: 30.0, score }
    }

    #[test]
    fn a_sure_hit_shows_at_once_and_an_unsure_one_needs_a_second_sighting() {
        let mut t = Tracker::new();
        assert!(t.discovery(&[hit(Kind::Cross, 100.0, 0.95)], 640, 360));
        assert_eq!(t.detections().len(), 1);
        let mut u = Tracker::new();
        assert!(!u.discovery(&[hit(Kind::Cross, 100.0, 0.8)], 640, 360), "0.8 is not sure enough on its own");
        assert!(u.detections().is_empty());
        assert!(u.discovery(&[hit(Kind::Cross, 101.0, 0.8)], 640, 360));
        assert_eq!(u.detections().len(), 1);
    }

    #[test]
    fn an_unconfirmed_hit_is_forgotten_and_a_shown_one_survives_missed_scans() {
        let mut u = Tracker::new();
        u.discovery(&[hit(Kind::Cross, 100.0, 0.8)], 640, 360);
        u.discovery(&[], 640, 360);
        u.discovery(&[], 640, 360);
        assert!(u.tracks.is_empty(), "noise that is not seen again goes");
        let mut t = Tracker::new();
        t.discovery(&[hit(Kind::Cross, 100.0, 0.95)], 640, 360);
        for _ in 0..DROP_AFTER_MISSED_SCANS - 1 {
            t.discovery(&[], 640, 360);
        }
        assert_eq!(t.detections().len(), 1, "only the quick checks take a shown glyph away, quickly");
        t.discovery(&[], 640, 360);
        assert!(t.detections().is_empty(), "but a glyph no scan finds for long is dropped as well");
    }

    #[test]
    fn a_glyph_keeps_its_id_while_it_is_followed_and_a_hit_far_away_is_a_new_one() {
        let mut t = Tracker::new();
        t.discovery(&[hit(Kind::Cross, 100.0, 0.95)], 640, 360);
        let id = t.detections()[0].id;
        t.discovery(&[hit(Kind::Cross, 102.0, 0.95)], 640, 360);
        assert_eq!(t.detections()[0].id, id);
        t.discovery(&[hit(Kind::Cross, 102.0, 0.95), hit(Kind::Cross, 400.0, 0.95)], 640, 360);
        let ids: Vec<u64> = t.detections().iter().map(|d| d.id).collect();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&id));
        let other = t.detections().iter().map(|d| d.kind).collect::<Vec<_>>();
        assert_eq!(other, [Kind::Cross, Kind::Cross]);
    }

    #[test]
    fn stability_tells_a_moving_scene_from_a_still_one() {
        let (w, h) = (64usize, 48usize);
        let mut st = Stability::new();
        let still = vec![90u8; w * h];
        for _ in 0..12 {
            st.update(&Plane { data: &still, w, h });
        }
        assert!(!st.scene_is_moving());
        let mut st = Stability::new();
        for k in 0..12usize {
            let f: Vec<u8> = (0..w * h).map(|i| if ((i % w) + k * 3) / 4 % 2 == 0 { 40 } else { 200 }).collect();
            st.update(&Plane { data: &f, w, h });
        }
        assert!(st.scene_is_moving());
        // a block that never changes survives in the HUD-only picture, the moving part is blanked
        let mut st = Stability::new();
        let mut last = vec![];
        for k in 0..12usize {
            last = (0..w * h)
                .map(|i| {
                    if i % w < 16 {
                        255
                    } else if ((i % w) + k * 3) / 4 % 2 == 0 {
                        40
                    } else {
                        200
                    }
                })
                .collect();
            st.update(&Plane { data: &last, w, h });
        }
        let hud = st.hud_only(&Plane { data: &last, w, h }, 0);
        assert_eq!(hud[5 * w + 4], 255, "the still block is kept");
        assert_eq!(hud[5 * w + 40], 0, "the moving part is blanked");
    }
}
