//! Spike: can we open the DualSense next to GameController and write an output report? Sets the lightbar.
use hidapi::HidApi;
fn main() {
    let api = HidApi::new().expect("hid");
    for d in api.device_list().filter(|d| d.vendor_id() == 0x054C) {
        println!(
            "{:04x}:{:04x} {:?} iface {} usage {:04x}/{:04x} bus {:?}",
            d.vendor_id(),
            d.product_id(),
            d.product_string(),
            d.interface_number(),
            d.usage_page(),
            d.usage(),
            d.bus_type()
        );
    }
    let Some(info) = api.device_list().find(|d| d.vendor_id() == 0x054C && d.product_id() == 0x0CE6) else {
        println!("no DualSense");
        return;
    };
    let dev = match info.open_device(&api) {
        Ok(d) => d,
        Err(e) => {
            println!("open failed: {e}");
            return;
        }
    };
    println!("opened");
    let mut seq = 0u8;
    let mut send = |f: &dyn Fn(&mut [u8])| {
        let mut buf = [0u8; 78];
        buf[0] = 0x31;
        buf[1] = seq << 4;
        seq = (seq + 1) & 0x0F;
        buf[2] = 0x10;
        f(&mut buf[3..50]);
        let mut h = crc32fast::Hasher::new();
        h.update(&[0xA2]);
        h.update(&buf[..74]);
        buf[74..78].copy_from_slice(&h.finalize().to_le_bytes());
        println!("write: {:?}", dev.write(&buf));
    };
    let pause = |s: u64| std::thread::sleep(std::time::Duration::from_secs(s));
    println!("1. lightbar RED");
    send(&|c| {
        c[1] = 0x04;
        c[38] = 0x02;
        c[41] = 0x02;
        c[44] = 255;
        c[45] = 0;
        c[46] = 0;
    });
    pause(3);
    println!("2. lightbar BLUE");
    send(&|c| {
        c[1] = 0x04;
        c[44] = 0;
        c[45] = 0;
        c[46] = 255;
    });
    pause(3);
    println!("3. rumble strong (left/heavy) 1.5 s");
    send(&|c| {
        c[0] = 0x01 | 0x02;
        c[2] = 0;
        c[3] = 255;
    });
    pause(2);
    send(&|c| {
        c[0] = 0x01 | 0x02;
        c[2] = 0;
        c[3] = 0;
    });
    println!("4. rumble light (right) 1.5 s");
    send(&|c| {
        c[0] = 0x01 | 0x02;
        c[2] = 255;
        c[3] = 0;
    });
    pause(2);
    send(&|c| {
        c[0] = 0x01 | 0x02;
        c[2] = 0;
        c[3] = 0;
    });
    println!("5. RIGHT trigger stiff resistance (mode 0x01) for 6 s: press R2");
    send(&|c| {
        c[0] = 0x04;
        c[10] = 0x01;
        c[11] = 0x30;
        c[12] = 0xFF;
    });
    pause(6);
    println!("6. LEFT trigger vibration-ish (mode 0x02 pulse) for 6 s: press L2");
    send(&|c| {
        c[0] = 0x04 | 0x08;
        c[10] = 0;
        c[21] = 0x02;
        c[22] = 0x20;
        c[23] = 0xFF;
        c[24] = 0xFF;
    });
    pause(6);
    println!("7. triggers off");
    send(&|c| {
        c[0] = 0x04 | 0x08;
        c[10] = 0;
        c[21] = 0;
    });
    pause(1);
}
