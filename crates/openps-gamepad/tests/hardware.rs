//! Talks to a real DualSense. By hand, with one connected:
//! `cargo test -p openps-gamepad --test hardware -- --ignored --nocapture --test-threads=1`
use hidapi::{BusType, HidApi, HidDevice};

fn open() -> Option<(HidDevice, bool)> {
    let api = HidApi::new().ok()?;
    let info = api.device_list().find(|d| d.vendor_id() == 0x054C && matches!(d.product_id(), 0x0CE6 | 0x0DF2))?;
    let usb = matches!(info.bus_type(), BusType::Usb);
    Some((info.open_device(&api).ok()?, usb))
}

#[test]
#[ignore = "needs a DualSense"]
fn firmware_report() {
    let Some((dev, usb)) = open() else { return println!("no DualSense") };
    let mut buf = [0u8; 64];
    buf[0] = 0x20;
    let n = dev.get_feature_report(&mut buf);
    println!("bus {}, feature 0x20: {n:?}", if usb { "usb" } else { "bluetooth" });
    for (i, row) in buf.chunks(16).enumerate() {
        println!("{:02x}: {}", i * 16, row.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" "));
    }
}

struct Stderr;
impl log::Log for Stderr {
    fn enabled(&self, _: &log::Metadata) -> bool {
        true
    }
    fn log(&self, r: &log::Record) {
        eprintln!("[{}] {}", r.level(), r.args());
    }
    fn flush(&self) {}
}

fn tone(frame_index: usize, hz: f64, amplitude: f64) -> [u8; 120] {
    let mut f = [0u8; 120];
    for (i, pair) in f.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let t = (frame_index * 30 + i) as f64 / 3000.0;
        let v = ((t * hz * std::f64::consts::TAU).sin() * amplitude * 32767.0) as i16;
        pair[..2].copy_from_slice(&v.to_le_bytes());
        pair[2..].copy_from_slice(&v.to_le_bytes());
    }
    f
}

/// Plays 2 s of a 150 Hz tone as haptics, then 1 s of console-style rumble, then stops. The pad should buzz (Bluetooth: voice coils;
/// USB: motors), then rumble, then go quiet. Watch stderr for warnings.
#[test]
#[ignore = "needs a DualSense and makes it vibrate"]
fn haptics_then_rumble_then_silence() {
    use openps_gamepad::hid::HidOut;
    use openps_stream::padblob::PadOutput;
    let _ = log::set_logger(&Stderr).map(|()| log::set_max_level(log::LevelFilter::Info));
    let hid = HidOut::start();
    hid.apply(0, &PadOutput::Led { pad: 0, r: 0, g: 40, b: 255 });
    hid.apply(0, &PadOutput::Intensity { haptic: 1, adaptive_trigger: 1 });
    std::thread::sleep(std::time::Duration::from_millis(500));
    println!("haptics: 150 Hz for 2 s");
    let start = std::time::Instant::now();
    for i in 0..200 {
        hid.haptic(0, tone(i, 150.0, 0.8));
        std::thread::sleep(
            (start + std::time::Duration::from_millis(10 * (i as u64 + 1)))
                .saturating_duration_since(std::time::Instant::now()),
        );
    }
    std::thread::sleep(std::time::Duration::from_millis(600));
    println!("rumble: 1 s");
    hid.apply(0, &PadOutput::Rumble { pad: 0, left: 180, right: 180 });
    std::thread::sleep(std::time::Duration::from_secs(1));
    hid.apply(0, &PadOutput::Rumble { pad: 0, left: 0, right: 0 });
    std::thread::sleep(std::time::Duration::from_millis(300));
    hid.clear(0);
    std::thread::sleep(std::time::Duration::from_millis(300));
    println!("done");
}
