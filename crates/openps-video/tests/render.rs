#![cfg(target_os = "macos")]
use openps_video::nal::{self, Codec, ParameterSets};
use openps_video::render::Renderer;
use openps_video::vt::Decoder;

#[test]
fn decoded_pictures_enqueue_without_failing_the_layer() {
    let data = |n: &str| std::fs::read(format!("{}/tests/data/{n}", env!("CARGO_MANIFEST_DIR"))).unwrap();
    for (name, codec) in [("h264.annexb", Codec::H264), ("hevc.annexb", Codec::Hevc)] {
        let s = data(name);
        let r = Renderer::new().expect("layer");
        assert!(!r.layer_ptr().is_null());
        let mut dec = Decoder::new(codec, ParameterSets::from_annex_b(codec, &s)).unwrap();
        let mut shown = 0;
        let mut have = false;
        let mut au: Vec<u8> = vec![];
        let flush = |au: &mut Vec<u8>, dec: &mut Decoder, shown: &mut u32| {
            if au.is_empty() {
                return;
            }
            for p in dec.decode_annex_b(au, 0).unwrap() {
                r.show(&p).unwrap_or_else(|e| panic!("{name}: {e:?}"));
                *shown += 1;
            }
            au.clear();
        };
        for n in nal::split(&s) {
            let k = nal::classify(codec, n);
            let pic = matches!(k, nal::Kind::Idr | nal::Kind::Slice);
            if have && (pic || matches!(k, nal::Kind::Sps | nal::Kind::Vps | nal::Kind::Pps)) {
                flush(&mut au, &mut dec, &mut shown);
                have = false;
            }
            have |= pic;
            au.extend([0, 0, 0, 1]);
            au.extend(n);
        }
        flush(&mut au, &mut dec, &mut shown);
        assert_eq!(shown, 30, "{name}");
        assert_ne!(r.status(), 2, "{name}: layer must not be in the failed state");
        r.flush();
    }
}
