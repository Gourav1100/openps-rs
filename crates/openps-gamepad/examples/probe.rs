//! Prints what macOS GameController reports for connected pads (hardware check, not shipped).
#[cfg(target_os = "macos")]
fn main() {
    use objc2_foundation::{NSDate, NSRunLoop};
    use objc2_game_controller::{GCController, GCDevice};
    unsafe {
        GCController::setShouldMonitorBackgroundEvents(true);
        let rl = NSRunLoop::currentRunLoop();
        for round in 0..6 {
            rl.runUntilDate(&NSDate::dateWithTimeIntervalSinceNow(0.5));
            let cs = GCController::controllers();
            println!("round {round}: {} controller(s)", cs.len());
            if round < 5 {
                continue;
            }
            for c in cs.iter() {
                println!("  {:?} category {:?}", c.vendorName(), c.productCategory());
                println!(
                    "  extended: {}  motion: {}  light: {}  haptics: {}  battery: {}",
                    c.extendedGamepad().is_some(),
                    c.motion().is_some(),
                    c.light().is_some(),
                    c.haptics().is_some(),
                    c.battery().is_some()
                );
                let p = c.physicalInputProfile();
                let mut b: Vec<String> = p.buttons().allKeys().iter().map(|k| k.to_string()).collect();
                b.sort();
                println!("  buttons: {b:?}");
                let mut a: Vec<String> = p.axes().allKeys().iter().map(|k| k.to_string()).collect();
                a.sort();
                println!("  axes: {a:?}");
                let mut d: Vec<String> = p.dpads().allKeys().iter().map(|k| k.to_string()).collect();
                d.sort();
                println!("  dpads: {d:?}");
                let mut t: Vec<String> = p.touchpads().allKeys().iter().map(|k| k.to_string()).collect();
                t.sort();
                println!("  touchpads: {t:?}");
                if let Some(h) = c.haptics() {
                    println!("  haptic localities: {}", h.supportedLocalities().len());
                }
                if let Some(m) = c.motion() {
                    println!(
                        "  motion manual activation: {} gravity: {} attitude: {} rate: {}",
                        m.sensorsRequireManualActivation(),
                        m.hasGravityAndUserAcceleration(),
                        m.hasAttitude(),
                        m.hasRotationRate()
                    );
                }
            }
        }
    }
}
#[cfg(not(target_os = "macos"))]
fn main() {}
