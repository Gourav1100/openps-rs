//! Deeply nested TOML (untrusted-size user file): the parser recurses on the native stack.
//! The child-process pattern is used because a stack overflow aborts the whole process (cannot be caught).
use openps_input::load_layers;
use std::process::Command;

fn nested(kind: char, depth: usize) -> String {
    match kind {
        'u' => format!("a = {}", "[".repeat(depth)),
        'v' => format!("a = {}", "{b=".repeat(depth)),
        'a' => format!("a = {}{}", "[".repeat(depth), "]".repeat(depth)),
        'i' => format!("a = {}1{}", "[".repeat(depth), "]".repeat(depth)),
        _ => format!("a = {}{}", "{b=".repeat(depth), "}".repeat(depth)),
    }
}

/// Child entry point: only does something when the env var is set.
#[test]
fn child_probe() {
    let Ok(spec) = std::env::var("TESTER_DEEP") else { return };
    let (kind, depth) = (spec.chars().next().unwrap(), spec[1..].parse::<usize>().unwrap());
    let stack: usize = std::env::var("TESTER_STACK").ok().and_then(|s| s.parse().ok()).unwrap_or(8 << 20);
    let t = std::thread::Builder::new().stack_size(stack).spawn(move || {
        let r = load_layers(Some(&nested(kind, depth)), true);
        println!("RESULT {}", if r.is_ok() { "ok" } else { "err" });
    });
    t.unwrap().join().unwrap();
}

fn probe(kind: char, depth: usize, stack: usize) -> (bool, String) {
    let out = Command::new(std::env::current_exe().unwrap())
        .args(["child_probe", "--exact", "--nocapture", "--test-threads=1"])
        .env("TESTER_DEEP", format!("{kind}{depth}"))
        .env("TESTER_STACK", stack.to_string())
        .output()
        .unwrap();
    (out.status.success(), String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr))
}

#[test]
fn moderately_nested_toml_is_rejected_cleanly() {
    for kind in ['a', 'i', 't'] {
        let (ok, out) = probe(kind, 100, 8 << 20);
        assert!(ok, "{kind} depth 100: {out}");
    }
}

#[test]
fn deeply_nested_toml_is_an_error_on_a_main_sized_stack() {
    for kind in ['a', 'i', 't', 'u', 'v'] {
        let (ok, out) = probe(kind, 100_000, 8 << 20);
        assert!(ok && out.contains("RESULT err"), "{kind}: {out}");
    }
}

#[test]
fn deeply_nested_toml_is_an_error_on_a_2mib_thread_stack() {
    for kind in ['u', 'a', 't'] {
        let (ok, out) = probe(kind, 1_000, 2 << 20);
        assert!(ok && out.contains("RESULT err"), "{kind}: {out}");
    }
}
