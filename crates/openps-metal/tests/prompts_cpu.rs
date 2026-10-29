//! The prompt detector against real glyph art (Kenney's CC0 PlayStation prompts) and real stream frames.
use openps_metal::prompts::*;

fn detect_with(p: &[u8], w: usize, h: usize, t: &Templates) -> Vec<Hit> {
    openps_metal::prompts::detect(&Plane { data: p, w, h }, t)
}

fn gray(name: &str) -> (Vec<u8>, usize, usize) {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data");
    let file = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(Result::ok)
        .find(|f| f.file_name().to_string_lossy().starts_with(name))
        .unwrap();
    let n = file.file_name().to_string_lossy().to_string();
    let dims = n.trim_end_matches(".gray").rsplit('_').next().unwrap().to_string();
    let (w, h) = dims.split_once('x').unwrap();
    (std::fs::read(file.path()).unwrap(), w.parse().unwrap(), h.parse().unwrap())
}

/// Rewrites `src/prompt_real.bin` from the live stream plane in `tests/data` (run by hand): the glyphs of that frame as templates, taken
/// exactly the way the detector will read them.
#[test]
#[ignore = "generator"]
fn generate_real_templates() {
    let t = Templates::new();
    let (p, w, h) = gray("live_plane");
    let plane = Plane { data: &p, w, h };
    // kind, x, y of glyphs seen in that frame
    let known = [
        (Kind::L1, 54.0, 36.0),
        (Kind::R1, 208.0, 36.0),
        (Kind::L2, 55.0, 62.0),
        (Kind::R2, 247.0, 62.0),
        (Kind::L3, 754.0, 487.0),
        (Kind::Circle, 71.0, 509.0),
        (Kind::LStick, 153.0, 509.0),
        (Kind::RStick, 225.0, 509.0),
        (Kind::Cross, 304.0, 509.0),
        (Kind::Triangle, 423.0, 509.0),
    ];
    let _ = &t;
    let mut boxes: Vec<(f32, f32, f32, f32)> =
        filled_candidates(&plane).into_iter().map(|(x, y, bw, bh)| (x, y, bw + 1.0, bh + 1.0)).collect();
    boxes.extend(badge_candidates(&plane, &t).into_iter().map(|(x, y, r, _)| (x, y, r * 2.0, r * 2.0)));
    let mut out = vec![];
    for (kind, x, y) in known {
        // the nearest box of the right size (bright discs and pills are found by both finders: keep both readings)
        for b in boxes.iter().filter(|b| (b.0 - x).abs() < 3.0 && (b.1 - y).abs() < 3.0 && b.2 < 40.0) {
            let patch = box_patch(&plane, b.0, b.1, b.2, b.3);
            let (lo, hi) = patch.iter().fold((f32::MAX, f32::MIN), |(l, h), v| (l.min(*v), h.max(*v)));
            let idx = Kind::ALL.iter().position(|k| *k == kind).unwrap() as u8;
            out.push(idx);
            out.extend((b.2 / b.3).to_le_bytes());
            out.extend(patch.iter().map(|v| ((v - lo) / (hi - lo) * 255.0).round() as u8));
        }
    }
    std::fs::write(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/prompt_real.bin"), out).unwrap();
}

// ---- scenes made of Kenney's real glyph art ----------------------------------------------------------------------------------------------

/// `(kind, 64x64 light-on-black luma, 64x64 alpha)` of one glyph per kind.
fn scene_art() -> Vec<(Kind, Vec<f32>, Vec<f32>)> {
    let bytes =
        std::fs::read(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/glyph_scenes.bin")).unwrap();
    bytes
        .chunks(1 + 2 * 64 * 64)
        .map(|r| {
            (
                Kind::ALL[usize::from(r[0])],
                r[1..4097].iter().map(|b| f32::from(*b)).collect(),
                r[4097..].iter().map(|b| f32::from(*b) / 255.0).collect(),
            )
        })
        .collect()
}

#[derive(Clone, Copy, Debug)]
enum Back {
    Dark,
    Mid,
    Ramp,
    Texture,
}

/// A 220x120 picture with the glyph (its longer side `size` pixels) centred at `(110, 60)` over the background.
fn scene(art: &(Kind, Vec<f32>, Vec<f32>), size: f32, back: Back) -> Vec<u8> {
    let (w, h) = (220usize, 120usize);
    let mut seed = 12345u32;
    let mut rnd = move || {
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        (seed >> 24) as f32 / 255.0
    };
    let mut bg = vec![0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            bg[y * w + x] = match back {
                Back::Dark => 28.0,
                Back::Mid => 95.0,
                Back::Ramp => 20.0 + 80.0 * x as f32 / w as f32 + 30.0 * y as f32 / h as f32,
                Back::Texture => 45.0 + 22.0 * rnd() + 14.0 * (((x / 5 + y / 5) % 2) as f32),
            };
        }
    }
    // the glyph's tight box inside the 64x64 art
    let (mut x0, mut x1, mut y0, mut y1) = (64usize, 0usize, 64usize, 0usize);
    for y in 0..64 {
        for x in 0..64 {
            if art.2[y * 64 + x] > 0.4 {
                (x0, x1, y0, y1) = (x0.min(x), x1.max(x), y0.min(y), y1.max(y));
            }
        }
    }
    let (gw, gh) = ((x1 - x0 + 1) as f32, (y1 - y0 + 1) as f32);
    let k = size / gw.max(gh);
    let (ow, oh) = (gw * k, gh * k);
    for y in 0..h {
        for x in 0..w {
            let (u, v) = (x as f32 + 0.5 - (110.0 - ow / 2.0), y as f32 + 0.5 - (60.0 - oh / 2.0));
            if u < -1.0 || v < -1.0 || u > ow + 1.0 || v > oh + 1.0 {
                continue;
            }
            // average the art over this output pixel's footprint (a 3x3 sub-sample grid)
            let (mut l, mut a) = (0.0, 0.0);
            for sy in 0..3 {
                for sx in 0..3 {
                    let su = (u - 0.5 + (sx as f32 + 0.5) / 3.0) / k + x0 as f32;
                    let sv = (v - 0.5 + (sy as f32 + 0.5) / 3.0) / k + y0 as f32;
                    let (ix, iy) = (su.floor() as isize, sv.floor() as isize);
                    if ix >= 0 && iy >= 0 && ix < 64 && iy < 64 {
                        l += art.1[iy as usize * 64 + ix as usize];
                        a += art.2[iy as usize * 64 + ix as usize];
                    }
                }
            }
            let i = y * w + x;
            bg[i] = bg[i] * (1.0 - a / 9.0) + l / 9.0;
        }
    }
    bg.iter().map(|v| v.round().clamp(0.0, 255.0) as u8).collect()
}

/// Is the glyph among the hits? A glyph made of a bar and a mark (Options, Create) is found at its bar, so it may sit off the centre.
fn found(hits: &[Hit], kind: Kind, size: f32) -> bool {
    let slack = if matches!(kind, Kind::Options | Kind::Create | Kind::Mute) { size * 0.5 } else { 4.0 };
    hits.iter().any(|d| d.kind == kind && (d.x - 110.0).abs() < 4.0 && (d.y - 60.0).abs() < slack)
}

fn scene_grid(kind: Kind) -> Vec<(f32, Back, Vec<Hit>)> {
    let t = Templates::new();
    let art = scene_art().into_iter().find(|a| a.0 == kind).unwrap();
    let mut out = vec![];
    for size in [12.0, 16.0, 22.0, 32.0, 48.0] {
        for back in [Back::Dark, Back::Mid, Back::Ramp, Back::Texture] {
            out.push((size, back, detect_with(&scene(&art, size, back), 220, 120, &t)));
        }
    }
    out
}

#[test]
fn the_main_glyphs_are_found_at_every_size_on_every_background() {
    for kind in [
        Kind::Cross,
        Kind::Circle,
        Kind::Square,
        Kind::Triangle,
        Kind::L1,
        Kind::R1,
        Kind::L2,
        Kind::R2,
        Kind::L3,
        Kind::R3,
        Kind::DPad,
    ] {
        for (size, back, hits) in scene_grid(kind) {
            assert!(found(&hits, kind, size), "{kind:?} at {size}px on {back:?}: {hits:?}");
            assert_eq!(hits.len(), 1, "{kind:?} at {size}px on {back:?} gave extras: {hits:?}");
        }
    }
}

#[test]
fn the_harder_glyphs_are_found_often_and_never_read_as_something_else() {
    // (kind, least share of the 20 scenes in which it must be found)
    for (kind, least) in [(Kind::LStick, 0.55), (Kind::RStick, 0.4), (Kind::Options, 0.65), (Kind::Create, 0.65)] {
        let grid = scene_grid(kind);
        let hit = grid.iter().filter(|(size, _, hits)| found(hits, kind, *size)).count();
        assert!(hit as f32 / grid.len() as f32 >= least, "{kind:?}: found in {hit} of {} scenes", grid.len());
        for (size, back, hits) in &grid {
            assert!(hits.iter().all(|d| d.kind == kind), "{kind:?} at {size}px on {back:?} was read as {hits:?}");
        }
    }
}

#[test]
fn flat_noisy_and_stripey_pictures_find_nothing() {
    let (w, h) = (320, 180);
    let t = Templates::new();
    let mut seed = 7u32;
    let mut rnd = move || {
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        (seed >> 24) as u8
    };
    let flat = vec![90u8; w * h];
    let noise: Vec<u8> = (0..w * h).map(|_| rnd()).collect();
    let stripes: Vec<u8> = (0..w * h).map(|i| if (i % w / 6) % 2 == 0 { 30 } else { 220 }).collect();
    let checker: Vec<u8> = (0..w * h).map(|i| if ((i % w / 9) + (i / w / 9)) % 2 == 0 { 20 } else { 235 }).collect();
    for (name, p) in [("flat", flat), ("noise", noise), ("stripes", stripes), ("checker", checker)] {
        assert!(detect_with(&p, w, h, &t).is_empty(), "{name}");
    }
}

#[test]
fn the_real_ps5_menu_prompts_are_found_and_nothing_else() {
    // a crop of a real PS5 game menu (screenshot of the stream): "(X) Continue" in a grey disc and "(O) Back" in a double ring
    let t = Templates::new();
    let (p, w, h) = gray("ps5_menu");
    let hits = detect_with(&p, w, h, &t);
    assert_eq!(hits.len(), 2, "{hits:?}");
    let cross = hits.iter().find(|d| d.kind == Kind::Cross).expect("the cross prompt");
    let circle = hits.iter().find(|d| d.kind == Kind::Circle).expect("the circle prompt");
    assert!((cross.x - 39.0).abs() < 4.0 && (cross.y - 85.0).abs() < 4.0, "{cross:?}");
    assert!((circle.x - 39.0).abs() < 4.0 && (circle.y - 512.0).abs() < 4.0, "{circle:?}");
}

#[test]
fn real_game_and_nature_pictures_have_no_false_prompts() {
    let t = Templates::new();
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data");
    let mut n = 0;
    for f in std::fs::read_dir(dir).unwrap().filter_map(Result::ok) {
        let name = f.file_name().to_string_lossy().to_string();
        if !name.starts_with("fp_") {
            continue;
        }
        n += 1;
        let (p, w, h) = gray(&name);
        let hits = detect_with(&p, w, h, &t);
        assert!(hits.is_empty(), "{name}: {:?}", hits.iter().map(|d| (d.kind, d.x, d.y, d.score)).collect::<Vec<_>>());
    }
    assert!(n >= 2, "fixtures missing");
}

#[test]
fn a_real_map_screen_gives_every_prompt_and_nothing_else() {
    // a 960x540 luma plane read from a live stream (a map screen): L1 R1 pills and L2 R2 shields on top, an L3 disc on the right, and a
    // row of prompts at the bottom: circle, left stick, right stick, cross, triangle
    let t = Templates::new();
    let (p, w, h) = gray("live_plane");
    let hits = detect_with(&p, w, h, &t);
    let want = [
        (Kind::L1, 54.0, 36.0),
        (Kind::R1, 208.0, 36.0),
        (Kind::L2, 55.0, 62.0),
        (Kind::R2, 247.0, 62.0),
        (Kind::L3, 754.0, 487.0),
        (Kind::Circle, 71.0, 509.0),
        (Kind::LStick, 153.0, 509.0),
        (Kind::RStick, 225.0, 509.0),
        (Kind::Cross, 304.0, 509.0),
        (Kind::Triangle, 423.0, 509.0),
    ];
    for (k, x, y) in want {
        assert!(
            hits.iter().any(|d| d.kind == k && (d.x - x).abs() < 4.0 && (d.y - y).abs() < 4.0),
            "{k:?} missing: {hits:?}"
        );
    }
    assert_eq!(hits.len(), want.len(), "nothing else: {hits:?}");
}

#[test]
fn a_busy_picture_is_scanned_quickly() {
    let t = Templates::new();
    let (p, w, h) = gray("live_plane");
    let t0 = std::time::Instant::now();
    for _ in 0..5 {
        detect_with(&p, w, h, &t);
    }
    let each = t0.elapsed() / 5;
    eprintln!("{w}x{h} busy map screen: {each:?} per scan");
    assert!(each < std::time::Duration::from_millis(if cfg!(debug_assertions) { 2500 } else { 60 }), "{each:?}");
}

// ---- in game: a prompt over a moving scene, and how fast it comes and goes ----------------------------------------------------------------

/// Composite the glyph art over `bg` (w x h luma) at `alpha`, its longer side `size` px, centred at `(cx, cy)`.
#[allow(clippy::too_many_arguments)]
fn paste_art(
    bg: &mut [f32],
    w: usize,
    h: usize,
    art: &(Kind, Vec<f32>, Vec<f32>),
    size: f32,
    cx: f32,
    cy: f32,
    alpha: f32,
) {
    let (mut x0, mut x1, mut y0, mut y1) = (64usize, 0usize, 64usize, 0usize);
    for y in 0..64 {
        for x in 0..64 {
            if art.2[y * 64 + x] > 0.4 {
                (x0, x1, y0, y1) = (x0.min(x), x1.max(x), y0.min(y), y1.max(y));
            }
        }
    }
    let (gw, gh) = ((x1 - x0 + 1) as f32, (y1 - y0 + 1) as f32);
    let k = size / gw.max(gh);
    let (ow, oh) = (gw * k, gh * k);
    for y in 0..h {
        for x in 0..w {
            let (u, v) = (x as f32 + 0.5 - (cx - ow / 2.0), y as f32 + 0.5 - (cy - oh / 2.0));
            if u < -1.0 || v < -1.0 || u > ow + 1.0 || v > oh + 1.0 {
                continue;
            }
            let (mut l, mut a) = (0.0, 0.0);
            for sy in 0..3 {
                for sx in 0..3 {
                    let su = (u - 0.5 + (sx as f32 + 0.5) / 3.0) / k + x0 as f32;
                    let sv = (v - 0.5 + (sy as f32 + 0.5) / 3.0) / k + y0 as f32;
                    let (ix, iy) = (su.floor() as isize, sv.floor() as isize);
                    if ix >= 0 && iy >= 0 && ix < 64 && iy < 64 {
                        l += art.1[iy as usize * 64 + ix as usize];
                        a += art.2[iy as usize * 64 + ix as usize];
                    }
                }
            }
            let i = y * w + x;
            bg[i] = bg[i] * (1.0 - alpha * a / 9.0) + alpha * l / 9.0;
        }
    }
}

type Art = (Kind, Vec<f32>, Vec<f32>);

/// Frame `k` of a real photograph scrolling past (as a game's scene does), with the glyph drawn on top where the HUD would be.
fn moving_frame(photo: &[u8], pw: usize, ph: usize, k: usize, glyph: Option<(&Art, f32)>) -> Vec<u8> {
    let (w, h) = (pw, ph);
    let mut bg = vec![0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            bg[y * w + x] = f32::from(photo[((y + k * 2) % ph) * pw + (x + k * 3) % pw]);
        }
    }
    if let Some((art, alpha)) = glyph {
        paste_art(&mut bg, w, h, art, 24.0, w as f32 / 2.0, h as f32 / 2.0, alpha);
    }
    bg.iter().map(|v| v.round().clamp(0.0, 255.0) as u8).collect()
}

#[test]
fn a_prompt_drawn_over_a_moving_scene_is_found_by_using_what_the_last_frames_showed() {
    let t = Templates::new();
    let (photo, pw, ph) = gray("fp_bird_005");
    let kinds = [Kind::Cross, Kind::Circle, Kind::Triangle, Kind::L1, Kind::R2, Kind::DPad];
    let (mut with_history, mut without) = (0, 0);
    for kind in kinds {
        let art = scene_art().into_iter().find(|a| a.0 == kind).unwrap();
        let mut st = Stability::new();
        let mut last = vec![];
        for k in 0..14 {
            last = moving_frame(&photo, pw, ph, k, Some((&art, 0.85)));
            st.update(&Plane { data: &last, w: pw, h: ph });
        }
        assert!(st.scene_is_moving(), "the photograph is scrolling");
        let plane = Plane { data: &last, w: pw, h: ph };
        let near = |hits: &[Hit]| {
            hits.iter()
                .any(|d| d.kind == kind && (d.x - pw as f32 / 2.0).abs() < 5.0 && (d.y - ph as f32 / 2.0).abs() < 5.0)
        };
        let hist = detect_with_history(&plane, &t, &st);
        let plain = detect_with(&last, pw, ph, &t);
        with_history += usize::from(near(&hist));
        without += usize::from(near(&plain));
        eprintln!(
            "{kind:?}: with history {} ({} hits), without {} ({} hits)",
            near(&hist),
            hist.len(),
            near(&plain),
            plain.len()
        );
    }
    eprintln!("found with history {with_history}/6, without {without}/6");
    assert!(with_history >= 5, "{with_history} of 6");
    assert!(with_history >= without);
}

#[test]
fn a_prompt_over_a_moving_scene_gives_no_false_prompts_when_there_is_none() {
    let t = Templates::new();
    let (photo, pw, ph) = gray("fp_bird_005");
    let mut st = Stability::new();
    let mut last = vec![];
    for k in 0..14 {
        last = moving_frame(&photo, pw, ph, k, None);
        st.update(&Plane { data: &last, w: pw, h: ph });
    }
    let hits = detect_with_history(&Plane { data: &last, w: pw, h: ph }, &t, &st);
    assert!(hits.is_empty(), "{hits:?}");
}

#[test]
fn a_glyph_shows_on_the_first_sure_scan_and_is_gone_two_frames_after_it_is_removed() {
    let t = Templates::new();
    let (w, h) = (320usize, 180usize);
    let art = scene_art().into_iter().find(|a| a.0 == Kind::Cross).unwrap();
    let draw = |on: bool| {
        let mut bg = vec![40f32; w * h];
        if on {
            paste_art(&mut bg, w, h, &art, 22.0, 160.0, 90.0, 1.0);
        }
        bg.iter().map(|v| *v as u8).collect::<Vec<u8>>()
    };
    let (with, without) = (draw(true), draw(false));
    let mut tr = Tracker::new();
    let hits = detect_with(&with, w, h, &t);
    assert!(tr.discovery(&hits, w, h), "one scan is enough for a sure glyph");
    assert_eq!(tr.detections().len(), 1);
    // it stays while it is there
    for _ in 0..5 {
        tr.verify(&Plane { data: &with, w, h }, &t);
    }
    assert_eq!(tr.detections().len(), 1);
    // and goes after two frames without it
    tr.verify(&Plane { data: &without, w, h }, &t);
    assert_eq!(tr.detections().len(), 1, "one bad frame is tolerated");
    assert!(tr.verify(&Plane { data: &without, w, h }, &t));
    assert!(tr.detections().is_empty());
}

#[test]
fn the_quick_check_is_cheap() {
    let t = Templates::new();
    let (p, w, h) = gray("live_plane");
    let plane = Plane { data: &p, w, h };
    let mut tr = Tracker::new();
    tr.discovery(&detect_with(&p, w, h, &t), w, h);
    assert!(tr.detections().len() >= 9);
    let t0 = std::time::Instant::now();
    for _ in 0..200 {
        tr.verify(&plane, &t);
    }
    let each = t0.elapsed() / 200;
    eprintln!("quick check of {} glyphs: {each:?}", tr.detections().len());
    assert!(each < std::time::Duration::from_micros(if cfg!(debug_assertions) { 20_000 } else { 1500 }), "{each:?}");
}
