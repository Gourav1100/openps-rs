//! Requests produced by the ORIGINAL application's own registration code (`com.grill.remoteplay.hb`, run on its bundled Java 25 runtime
//! against a local capture server, test account id 1234567890123456789, PIN 12345678, a different random body each time) are decoded
//! with OUR tables and derivation, the way a console would. If the nonce patch, the table indices or the cipher differed from the
//! original, the header block would come out as noise and the console would answer "wrong credentials".
//! Skips when the key tables are not available.
use base64::Engine as _;
use openps_discovery::DeviceType;
use openps_rp::kdf::{regist_cipher, Family};
use openps_rp::regist::{build_request, parse_header_block, RegisterParams};
use openps_rp::tables::KeyTables;
use rand_core::RngCore;

const PIN: u32 = 12_345_678;
const ACCOUNT: u64 = 1_234_567_890_123_456_789;

fn tables() -> Option<KeyTables> {
    if let Some(r) = KeyTables::from_env() {
        return Some(r.expect("OPENPS_TABLES invalid"));
    }
    KeyTables::load(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../analysis/tables/tables.json")).ok()
}

fn captures(prefix: &str) -> Vec<(String, Vec<u8>)> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/original");
    let mut v: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with(prefix))
        .map(|e| (e.file_name().to_string_lossy().to_string(), std::fs::read(e.path()).unwrap()))
        .collect();
    v.sort();
    v
}

fn split(raw: &[u8]) -> (String, Vec<u8>) {
    let i = raw.windows(4).position(|w| w == b"\r\n\r\n").expect("head end");
    (String::from_utf8_lossy(&raw[..i]).to_string(), raw[i + 4..].to_vec())
}

/// What a console does: undo the nonce patch, derive the key from the PIN, decrypt the header block.
fn console_decode(t: &KeyTables, version: &str, ps5: bool, body: &[u8]) -> std::collections::BTreeMap<String, String> {
    let new = matches!(version, "1.0" | "10.0");
    let (family, nonce) = if new {
        let idx_hi = (body[0] >> 3) as usize;
        let mut patch = [0u8; 16];
        patch[..8].copy_from_slice(&body[401..409]);
        patch[8..].copy_from_slice(&body[199..207]);
        let mut n = [0u8; 16];
        for i in 0..16 {
            n[i] = if ps5 {
                patch[i].wrapping_add(45).wrapping_sub(i as u8) ^ t.c[(i << 5) + idx_hi]
            } else {
                patch[i].wrapping_sub(41).wrapping_sub(i as u8) ^ t.b[(i << 5) + idx_hi]
            };
        }
        (if ps5 { Family::Ps5 } else { Family::Ps4 }, n)
    } else {
        let mut n = [0u8; 16];
        for i in 0..16 {
            n[i] = (t.a[i] ^ body[284 + i]).wrapping_add(i as u8).wrapping_add(41);
        }
        (Family::Legacy, n)
    };
    let idx = (body[397] & 0x1F) as usize;
    let c = regist_cipher(t, family, &nonce, PIN, idx);
    let mut enc = body[480..].to_vec();
    c.decrypt(&mut enc, 0);
    parse_header_block(&enc)
}

fn check(prefix: &str, version: &str, ps5: bool, expect_account: bool) {
    let Some(t) = tables() else {
        eprintln!("SKIPPED: key tables not available");
        return;
    };
    let caps = captures(prefix);
    assert_eq!(caps.len(), 10, "{prefix}");
    for (name, raw) in &caps {
        let (_, body) = split(raw);
        let h = console_decode(&t, version, ps5, &body);
        let want_client = if matches!(version, "1.0" | "10.0") { openps_rp::regist::CLIENT_TYPE } else { "Windows" };
        assert_eq!(
            h.get("Client-Type").map(String::as_str),
            Some(want_client),
            "{name}: header block did not decode: {h:?}"
        );
        if expect_account {
            let want = base64::engine::general_purpose::STANDARD.encode(ACCOUNT.to_le_bytes());
            assert_eq!(h.get("Np-AccountId"), Some(&want), "{name}");
        } else {
            assert_eq!(h.get("Np-Online-Id").map(String::as_str), Some("online"), "{name}");
        }
    }
}

#[test]
fn originals_ps5_1_0_decode_with_our_tables() {
    check("PS5_1.0_", "1.0", true, true);
}
#[test]
fn originals_ps4_10_0_decode_with_our_tables() {
    check("PS4_10.0_", "10.0", false, true);
}
#[test]
fn originals_ps4_9_0_decode_with_our_tables() {
    check("PS4_9.0_", "9.0", false, true);
}
#[test]
fn originals_ps4_8_0_decode_with_our_tables() {
    check("PS4_8.0_", "8.0", false, false);
}

struct Zero;
impl RngCore for Zero {
    fn next_u32(&mut self) -> u32 {
        0
    }
    fn next_u64(&mut self) -> u64 {
        0
    }
    fn fill_bytes(&mut self, d: &mut [u8]) {
        d.fill(7);
    }
    fn try_fill_bytes(&mut self, d: &mut [u8]) -> Result<(), rand_core::Error> {
        d.fill(7);
        Ok(())
    }
}

/// Our request has the same shape as the original's: request line, header names in the same order (the original's HTTP library adds
/// `Accept-Encoding`, which a console does not need), and a body of the same length.
#[test]
fn our_requests_have_the_same_shape_as_the_originals() {
    let Some(t) = tables() else { return };
    for (prefix, version, device) in [
        ("PS5_1.0_", "1.0", DeviceType::Ps5),
        ("PS4_10.0_", "10.0", DeviceType::Ps4),
        ("PS4_9.0_", "9.0", DeviceType::Ps4),
        ("PS4_8.0_", "8.0", DeviceType::Ps4),
    ] {
        let (_, raw) = &captures(prefix)[0];
        let (head, body) = split(raw);
        let ours = build_request(
            &t,
            &RegisterParams { version, device, pin: PIN, account_id: ACCOUNT, online_id: "online" },
            "127.0.0.1",
            &mut Zero,
        )
        .request;
        let theirs: Vec<&str> =
            head.lines().skip(1).map(|l| l.split(':').next().unwrap()).filter(|n| *n != "Accept-Encoding").collect();
        let mine_bytes = ours.to_bytes();
        let (mine_head, mine_body) = split(&mine_bytes);
        let mine: Vec<&str> = mine_head.lines().skip(1).map(|l| l.split(':').next().unwrap()).collect();
        assert_eq!(head.lines().next(), mine_head.lines().next(), "{prefix}: request line");
        assert_eq!(theirs, mine, "{prefix}: header names/order");
        assert_eq!(body.len(), mine_body.len(), "{prefix}: body length");
    }
}

/// Manual end-to-end check: decode a request captured from the RUNNING app.
/// `OPENPS_CAPTURE=<raw request file> OPENPS_PIN=<digits typed> OPENPS_VERSION=1.0 OPENPS_DEVICE=ps5 cargo test -p openps-rp --test original_requests -- --ignored --nocapture`
#[test]
#[ignore]
fn decode_a_capture_from_the_running_app() {
    let (Ok(file), Ok(pin)) = (std::env::var("OPENPS_CAPTURE"), std::env::var("OPENPS_PIN")) else { return };
    let t = tables().expect("tables");
    let raw = std::fs::read(file).unwrap();
    let (head, body) = split(&raw);
    println!("--- request head ---\n{head}\n--- body {} bytes ---", body.len());
    let version = std::env::var("OPENPS_VERSION").unwrap_or("1.0".into());
    let ps5 = std::env::var("OPENPS_DEVICE").unwrap_or("ps5".into()) == "ps5";
    let pin: u32 = pin.parse().unwrap();
    // same decode as `console_decode`, with the PIN that was typed
    let idx_hi = (body[0] >> 3) as usize;
    let mut patch = [0u8; 16];
    patch[..8].copy_from_slice(&body[401..409]);
    patch[8..].copy_from_slice(&body[199..207]);
    let mut n = [0u8; 16];
    for i in 0..16 {
        n[i] = if ps5 {
            patch[i].wrapping_add(45).wrapping_sub(i as u8) ^ t.c[(i << 5) + idx_hi]
        } else {
            patch[i].wrapping_sub(41).wrapping_sub(i as u8) ^ t.b[(i << 5) + idx_hi]
        };
    }
    let fam = if ps5 { Family::Ps5 } else { Family::Ps4 };
    let c = regist_cipher(&t, fam, &n, pin, (body[397] & 0x1F) as usize);
    let mut enc = body[480..].to_vec();
    c.decrypt(&mut enc, 0);
    let h = parse_header_block(&enc);
    println!("version {version}; decoded headers: {h:?}");
    if let Some(a) = h.get("Np-AccountId") {
        let b = base64::engine::general_purpose::STANDARD.decode(a).unwrap();
        println!("account id on the wire: {}", u64::from_le_bytes(b.try_into().unwrap()));
    }
    assert!(h.contains_key("Client-Type"), "did not decode with PIN {pin}");
}
