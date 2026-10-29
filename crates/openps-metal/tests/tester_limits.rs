//! Tester A5: texture-size limits. Kept in its own binary so that a Metal exception on an absurd size cannot take other tests down.
//! Run each test separately if you suspect an abort: `cargo test --test tester_limits <name>`.
#![cfg(target_os = "macos")]
use objc2_metal::*;
use openps_metal::gpu::{make_tex, Gpu};
use openps_metal::{MetalRenderer, RenderSettings};

fn blank(g: &Gpu, w: usize, h: usize) -> openps_metal::gpu::Tex {
    make_tex(&g.dev, MTLPixelFormat::BGRA8Unorm, w, h, MTLTextureUsage::ShaderRead, false).unwrap()
}

#[test]
fn the_widest_and_tallest_legal_textures_go_through_the_pipeline() {
    let mut g = Gpu::new().unwrap();
    for (w, h) in [(16384usize, 2usize), (2, 16384), (16384, 1), (1, 16384)] {
        let cmd = g.command_buffer().unwrap();
        g.ingest(&cmd, blank(&g, w, h), true).unwrap();
        g.ingest(&cmd, blank(&g, w, h), true).unwrap();
        let (t, up) = g.compose(&cmd, 0.5, None, (0.0, 0.0)).unwrap();
        assert!(!up);
        let tg = make_tex(&g.dev, MTLPixelFormat::BGRA8Unorm, 640, 360, MTLTextureUsage::RenderTarget, false).unwrap();
        g.present(&cmd, &t, &tg).unwrap();
        cmd.commit();
        cmd.waitUntilCompleted();
        assert_eq!(cmd.status(), MTLCommandBufferStatus::Completed, "{w}x{h}: {:?}", cmd.error());
    }
}

#[test]
fn an_8k_frame_goes_through_the_whole_pipeline_in_reasonable_time() {
    let mut g = Gpu::new().unwrap();
    let (w, h) = (7680usize, 4320usize);
    let t0 = std::time::Instant::now();
    let mut worst = 0.0f64;
    for _ in 0..4 {
        let s = std::time::Instant::now();
        let cmd = g.command_buffer().unwrap();
        g.ingest(&cmd, blank(&g, w, h), true).unwrap();
        let (t, _) = g.compose(&cmd, 0.5, None, (0.0, 0.0)).unwrap();
        let tg =
            make_tex(&g.dev, MTLPixelFormat::BGRA8Unorm, 3840, 2160, MTLTextureUsage::RenderTarget, false).unwrap();
        g.present(&cmd, &t, &tg).unwrap();
        cmd.commit();
        cmd.waitUntilCompleted();
        assert_eq!(cmd.status(), MTLCommandBufferStatus::Completed, "{:?}", cmd.error());
        worst = worst.max(s.elapsed().as_secs_f64() * 1e3);
    }
    eprintln!("8K ingest+warp+present: worst {worst:.0} ms, total {:?}", t0.elapsed());
    assert!(
        worst < 2000.0,
        "{worst} ms: a GPU watchdog (kIOGPUCommandBufferCallbackErrorImpactingInteractivity) is near"
    );
}

/// BUG T09-05: a frame size above Metal's 16384 limit must produce an `Err`, not a Metal validation assertion (SIGABRT of the whole
/// process: "MTLTextureDescriptor has width (20000) greater than the maximum allowed size of 16384"). `make_tex` is called with the
/// decoder's frame size in `renderer::make_ring` (6 textures per size change) and with derived sizes in `Gpu::ingest`.
/// WARNING: while the bug exists this test aborts the test process (run it alone).
#[test]
fn an_oversized_texture_is_an_error_not_an_abort() {
    let g = Gpu::new().unwrap();
    let r = make_tex(&g.dev, MTLPixelFormat::BGRA8Unorm, 20_000, 20_000, MTLTextureUsage::ShaderRead, false);
    assert!(r.is_err(), "20000x20000 must not be allocated");
    let r = make_tex(&g.dev, MTLPixelFormat::BGRA8Unorm, usize::MAX / 8, 4, MTLTextureUsage::ShaderRead, false);
    assert!(r.is_err());
}

#[test]
fn drawable_size_extremes_do_not_crash_the_layer_or_the_thread() {
    let r = MetalRenderer::new(Default::default(), RenderSettings::default()).unwrap();
    for (w, h) in [
        (0usize, 0usize),
        (1, 1),
        (1, 20_000),
        (20_000, 1),
        (16384, 16384),
        (100_000_000, 100_000_000),
        (usize::MAX, usize::MAX),
    ] {
        r.set_drawable_size(w, h);
        std::thread::sleep(std::time::Duration::from_millis(30));
    }
    r.set_drawable_size(960, 540);
    drop(r);
}

/// `OPENPS_DRAWABLES` must be restricted to 2 or 3 (CAMetalLayer raises an Objective-C exception for other values).
#[test]
fn the_drawable_count_override_must_be_two_or_three() {
    use openps_metal::renderer::drawable_override;
    assert_eq!(drawable_override(Some("2")), Some(2));
    assert_eq!(drawable_override(Some("3")), Some(3));
    for bad in ["1", "0", "4", "100", "-1", "x", ""] {
        assert_eq!(drawable_override(Some(bad)), None, "{bad}");
    }
    assert_eq!(drawable_override(None), None);
}
