//! The detector thread end to end: a real decoded-picture buffer in, a scene on the link out.
#![cfg(target_os = "macos")]

use openps_input::scene::SceneLink;
use openps_scene::find_models_dir;
use openps_scene::service::{SceneService, PICTURE_INTERVAL_S};
use std::ffi::c_void;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[link(name = "CoreVideo", kind = "framework")]
extern "C" {
    fn CVPixelBufferCreate(
        allocator: *const c_void,
        width: usize,
        height: usize,
        pixel_format: u32,
        attributes: *const c_void,
        out: *mut *const c_void,
    ) -> i32;
    fn CVPixelBufferRelease(b: *const c_void);
}

const BGRA: u32 = 0x4247_5241;
/// `420v`: what the decoder delivers when the system compositor shows the picture.
const NV12: u32 = 0x3432_3076;

fn picture(format: u32) -> *const c_void {
    let mut b = std::ptr::null();
    let status = unsafe { CVPixelBufferCreate(std::ptr::null(), 1920, 1080, format, std::ptr::null(), &mut b) };
    assert_eq!(status, 0);
    b
}

fn wait_for(link: &SceneLink, what: &str, mut done: impl FnMut(&SceneLink) -> bool) {
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(30) {
        if done(link) {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("gave up waiting for {what}: {:?}", link.status());
}

#[test]
fn a_picture_gives_a_scene_and_switching_off_clears_it() {
    a_picture_gives_a_scene(BGRA);
}

#[test]
fn the_decoders_native_format_works_too() {
    a_picture_gives_a_scene(NV12);
}

fn a_picture_gives_a_scene(format: u32) {
    if find_models_dir().is_none() {
        eprintln!("skipped: no model in assets/models");
        return;
    }
    let cache = tempfile::tempdir().unwrap();
    let link = Arc::new(SceneLink::new());
    let service = SceneService::start(link.clone(), cache.path().to_path_buf());
    let buffer = picture(format);
    unsafe { service.offer(buffer) }; // detection is off: ignored
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(link.status().context, None);

    link.set_enabled(true);
    assert_eq!(link.status().state, "loading");
    // keep offering: the first pictures may arrive while the model is still being compiled
    wait_for(&link, "a scene", |l| {
        unsafe { service.offer(buffer) };
        l.status().context.is_some()
    });
    let s = link.status();
    assert_eq!(s.state, "running");
    assert!(s.confidence > 0 && s.view.is_some() == matches!(s.context, Some("on_foot" | "in_vehicle")), "{s:?}");

    // pictures closer together than the interval are not all looked at
    let before = link.now();
    for _ in 0..20 {
        unsafe { service.offer(buffer) };
    }
    assert!(link.now() - before < PICTURE_INTERVAL_S * 20.0);

    link.set_enabled(false);
    assert_eq!(link.status().context, None);
    assert_eq!(link.status().state, "off");
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(link.status().state, "off", "a picture in flight must not bring it back");
    service.stop();
    unsafe { CVPixelBufferRelease(buffer) };
}

#[test]
fn without_a_model_it_says_so_instead_of_failing() {
    let cache = tempfile::tempdir().unwrap();
    let link = Arc::new(SceneLink::new());
    let service = SceneService::start_with(link.clone(), cache.path().to_path_buf(), None);
    link.set_enabled(true);
    wait_for(&link, "the notice", |l| l.status().state == "unavailable");
    assert!(link.status().note.contains("not installed"), "{:?}", link.status());
    assert_eq!(link.layer().name(), "on_foot");
    service.stop();
}

#[test]
fn a_load_that_is_told_to_stop_gives_up_and_leaves_nothing_behind() {
    let Some(models) = find_models_dir() else {
        eprintln!("skipped: no model in assets/models");
        return;
    };
    let cache = tempfile::tempdir().unwrap();
    let err = openps_scene::encoder::Encoder::load_until(&models, cache.path(), &|| true).err().expect("it gave up");
    assert_eq!(err, openps_scene::encoder::CANCELLED);
    assert_eq!(std::fs::read_dir(cache.path()).unwrap().count(), 0, "no compiled model, whole or partial");
}

#[test]
fn stopping_a_service_that_is_still_loading_does_not_wait_for_the_load() {
    if find_models_dir().is_none() {
        eprintln!("skipped: no model in assets/models");
        return;
    }
    let cache = tempfile::tempdir().unwrap(); // nothing compiled yet: the load takes seconds
    let link = Arc::new(SceneLink::new());
    let service = SceneService::start(link.clone(), cache.path().to_path_buf());
    let started = Instant::now();
    service.stop();
    assert!(started.elapsed() < Duration::from_secs(1), "stop took {:?}", started.elapsed());
}

#[test]
fn the_service_stops_promptly_and_survives_being_dropped_busy() {
    let cache = tempfile::tempdir().unwrap();
    let link = Arc::new(SceneLink::new());
    let started = Instant::now();
    {
        let _service = SceneService::start_with(link.clone(), cache.path().to_path_buf(), None);
        link.set_enabled(true);
    }
    assert!(started.elapsed() < Duration::from_secs(2));
}
