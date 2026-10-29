//! Our code against the ORIGINAL application's own classes (`com.grill.remoteplay.{gb,gc,fg,ff,gz}`), run on seeded inputs by
//! `tests/oracle/Gen.java` (see its header). `tests/data/oracle.jsonl` holds 12 cases (committed: it contains no cipher nonces, which
//! would reveal key-table bytes). For a byte-exact comparison set `OPENPS_REMOTE_EXACT=<exact.jsonl>` (made by the same tool).
use base64::Engine as _;
use openps_remote::keys::{build_register_request, decode_custom_data, register_cipher};
use openps_remote::rudp;
use openps_rp::regist::parse_header_block;
use openps_rp::tables::KeyTables;
use serde_json::Value;

fn tables() -> Option<KeyTables> {
    if let Some(r) = KeyTables::from_env() {
        return Some(r.expect("OPENPS_TABLES invalid"));
    }
    KeyTables::load(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../analysis/tables/tables.json")).ok()
}

fn cases(kind: &str) -> Vec<Value> {
    let text =
        std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/oracle.jsonl"))
            .unwrap();
    let v: Vec<Value> =
        text.lines().map(|l| serde_json::from_str(l).unwrap()).filter(|v: &Value| v["kind"] == kind).collect();
    assert_eq!(v.len(), 12, "{kind}");
    v
}

fn h(v: &Value, k: &str) -> Vec<u8> {
    let s = v[k].as_str().unwrap_or_else(|| panic!("{k}"));
    (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap()).collect()
}
fn a16(v: &Value, k: &str) -> [u8; 16] {
    h(v, k).try_into().unwrap()
}

#[test]
fn customdata1_decodes_to_the_same_key_as_the_original() {
    let Some(t) = tables() else { return eprintln!("SKIPPED: key tables not available") };
    let b64 = base64::engine::general_purpose::STANDARD;
    for c in cases("custom") {
        let custom = b64.encode(b64.encode(h(&c, "raw")));
        assert_eq!(decode_custom_data(&t, &a16(&c, "i"), &a16(&c, "j"), &custom).unwrap(), a16(&c, "l"));
    }
}

/// What a console does with the request: undo the nonce obfuscation, derive the key, decrypt the headers.
#[test]
fn requests_built_by_the_original_decode_with_our_derivation() {
    let Some(t) = tables() else { return eprintln!("SKIPPED: key tables not available") };
    for c in cases("register") {
        let req = h(&c, "request");
        let split = req.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        let (head, body) = (String::from_utf8_lossy(&req[..split]).to_string(), &req[split + 4..]);
        assert!(head.starts_with("POST /sie/ps5/rp/sess/rgst HTTP/1.1\r\nHOST: "), "{head}");
        assert!(head.contains(&format!("HOST: {}\r\n", c["ip"].as_str().unwrap())));
        assert!(head.contains("User-Agent: remoteplay Windows\r\nConnection: close\r\nRP-Version: 1.0\r\n"));
        assert!(head.ends_with(&format!("Content-Length: {}", body.len())));
        let l = a16(&c, "l");
        let n = (body[0] >> 3) as usize;
        let mut obf = [0u8; 16];
        obf[..8].copy_from_slice(&body[401..409]);
        obf[8..].copy_from_slice(&body[199..207]);
        let mut nonce = [0u8; 16];
        for i in 0..16 {
            nonce[i] = (obf[i] ^ t.c[(i << 5) + n]).wrapping_add(i as u8).wrapping_sub(43);
        }
        let cipher = register_cipher(&t, &l, (body[397] & 0x1F) as usize, nonce);
        let mut enc = body[480..].to_vec();
        cipher.decrypt(&mut enc, 0);
        let hdr = parse_header_block(&enc);
        let want = base64::engine::general_purpose::STANDARD
            .encode(c["account"].as_str().unwrap().parse::<u64>().unwrap().to_le_bytes());
        assert_eq!(
            hdr.get("Client-Type").map(String::as_str),
            Some(openps_rp::regist::CLIENT_TYPE),
            "did not decode: {hdr:?}"
        );
        assert_eq!(hdr.get("Np-AccountId"), Some(&want));
        // the random part is untouched except the two patched places
        let orig = h(&c, "body");
        for i in 0..480 {
            if !(199..207).contains(&i) && !(401..409).contains(&i) {
                assert_eq!(body[i], orig[i], "byte {i}");
            }
        }
    }
}

#[test]
fn our_request_is_byte_identical_to_the_originals_when_given_the_same_nonce() {
    let (Some(t), Ok(path)) = (tables(), std::env::var("OPENPS_REMOTE_EXACT")) else {
        return eprintln!(
            "SKIPPED: set OPENPS_REMOTE_EXACT=<exact.jsonl> (and have the key tables) for the byte-exact check"
        );
    };
    let mut n = 0;
    for line in std::fs::read_to_string(path).unwrap().lines() {
        let c: Value = serde_json::from_str(line).unwrap();
        let body: [u8; 480] = h(&c, "body").try_into().unwrap();
        let ip: std::net::Ipv4Addr = c["ip"].as_str().unwrap().parse().unwrap();
        let (req, _) = build_register_request(
            &t,
            &a16(&c, "l"),
            &body,
            a16(&c, "nonce"),
            c["account"].as_str().unwrap().parse().unwrap(),
            ip,
        );
        assert_eq!(req, h(&c, "request"), "case {n}");
        n += 1;
    }
    assert!(n >= 12);
}

#[test]
fn the_four_reliable_udp_datagrams_match_the_original_byte_for_byte() {
    for c in cases("rudp") {
        let (seq, tag, n3) =
            (c["seq"].as_u64().unwrap() as u16, c["tag"].as_u64().unwrap() as u32, c["n3"].as_u64().unwrap() as u16);
        assert_eq!(rudp::init(seq, tag), h(&c, "init"));
        assert_eq!(rudp::cookie(seq, tag, &h(&c, "cookie")), h(&c, "cookie_dg"));
        assert_eq!(rudp::request(seq, n3, &h(&c, "request")), h(&c, "request_dg"));
        assert_eq!(rudp::fin(seq, n3), h(&c, "final"));
        // and our parser reads them the way the original's does
        let first = rudp::parse_first_chunk(&h(&c, "cookie_dg")).unwrap();
        assert_eq!(first.ty, rudp::T_COOKIE);
        assert_eq!(&first.data[first.data.len() - h(&c, "cookie").len()..], &h(&c, "cookie")[..]);
    }
}

#[test]
fn customdata1_is_decoded_as_leniently_as_javas_decoder() {
    let Some(t) = tables() else { return eprintln!("SKIPPED: key tables not available") };
    let b64 = base64::engine::general_purpose::STANDARD;
    let case = &cases("custom")[0];
    let (i, j, raw, want) = (a16(case, "i"), a16(case, "j"), h(case, "raw"), a16(case, "l"));
    let padded = b64.encode(b64.encode(&raw));
    // padding stripped at either or both layers, and whitespace around it
    let strip = |s: &str| s.trim_end_matches('=').to_string();
    let unpadded_inner = b64.encode(strip(&b64.encode(&raw)));
    for (name, text) in [
        ("padded", padded.clone()),
        ("outer unpadded", strip(&padded)),
        ("inner unpadded", unpadded_inner.clone()),
        ("both unpadded", strip(&unpadded_inner)),
        ("surrounded by whitespace", format!("  {padded}\n")),
    ] {
        assert_eq!(decode_custom_data(&t, &i, &j, &text).unwrap(), want, "{name}");
    }
    // longer data: only the first 16 bytes matter (the original reads l[0..16])
    let mut longer = raw.clone();
    longer.extend([1, 2, 3, 4, 5, 6, 7, 8]);
    assert_eq!(decode_custom_data(&t, &i, &j, &b64.encode(b64.encode(&longer))).unwrap(), want);
}

#[test]
fn bad_customdata1_says_what_was_wrong_without_revealing_it() {
    let Some(t) = tables() else { return eprintln!("SKIPPED: key tables not available") };
    let b64 = base64::engine::general_purpose::STANDARD;
    let (i, j) = ([b'A'; 16], [b'B'; 16]);
    for (input, needle) in [
        ("!!!not base64!!!".to_string(), "outer layer"),
        (b64.encode([0xFF, 0xFE, 0xFD]), "not text"),
        (b64.encode("@@@@"), "inner layer"),
        (b64.encode(b64.encode([1u8; 10])), "10 bytes, need at least 16"),
        (String::new(), "0 bytes"),
    ] {
        let e = decode_custom_data(&t, &i, &j, &input).unwrap_err().to_string();
        assert!(e.contains(needle), "{input:?} -> {e}");
        assert!(!e.contains(&input) || input.is_empty() || input.len() < 6, "the data itself is never echoed: {e}");
    }
}
