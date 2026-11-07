//! Tester A1: resource-exhaustion probes for the video assembler, measured with a counting global allocator.
//! (Own test binary so the allocator hook does not disturb other tests.)
use openps_stream::video::{Unit, VideoAssembler, MAX_PENDING_FRAMES, MAX_UNITS};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

struct Counting;

thread_local! {
    // Per thread, so the test harness's own threads and the other tests running at the same time are not counted in a measurement.
    // A const-initialised `Cell` needs no allocation to set up, which a global allocator hook requires.
    static LIVE: Cell<isize> = const { Cell::new(0) };
}

fn add(n: isize) {
    let _ = LIVE.try_with(|c| c.set(c.get() + n));
}

fn live_now() -> isize {
    LIVE.with(Cell::get)
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = System.alloc(l);
        if !p.is_null() {
            add(l.size() as isize);
        }
        p
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        let p = System.alloc_zeroed(l);
        if !p.is_null() {
            add(l.size() as isize);
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        System.dealloc(p, l);
        add(-(l.size() as isize));
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        let q = System.realloc(p, l, n);
        if !q.is_null() {
            add(n as isize - l.size() as isize);
        }
        q
    }
}
#[global_allocator]
static A: Counting = Counting;

/// Feed `frames` distinct incomplete frames, each announcing `units` units with a huge pad header
/// (block = 65000 B) and only 2-byte payloads. Returns (wire bytes fed, bytes allocated live afterwards).
fn flood(frames: u16, units: u16, fec: u16) -> (usize, usize) {
    let mut a = VideoAssembler::new(3);
    let before = live_now();
    let mut wire = 0;
    let pad = 64_998u16.to_be_bytes();
    let k = units - fec;
    for f in 0..frames {
        for i in 0..k - 1 {
            // never send the last source unit: the frame cannot complete (and no parity => no FEC either)
            a.push(&Unit {
                frame_id: 1000 + f,
                unit_index: i,
                total_units: units,
                fec_units: fec,
                adaptive_index: 0,
                payload: &pad,
            });
            wire += 2 + 29;
        }
    }
    let live = (live_now() - before).max(0) as usize;
    drop(a);
    (wire, live)
}

#[test]
fn memory_amplification_of_incomplete_frames_is_bounded() {
    let (wire, live) = flood(2, MAX_UNITS as u16, 12);
    let ratio = live as f64 / wire as f64;
    println!("2 frames x {} units: {wire} B on the wire -> {} MiB live ({ratio:.0}x)", MAX_UNITS - 12, live >> 20);
    println!(
        "extrapolated worst case with {MAX_PENDING_FRAMES} pending frames: ~{} MiB",
        (live / 2 * MAX_PENDING_FRAMES) >> 20
    );
    // fixed: oversized blocks are rejected and units are stored at their real length
    assert!(live < 1 << 20, "{live} B live");
}

#[test]
fn incomplete_frames_do_not_amplify_memory() {
    let (wire, live) = flood(2, MAX_UNITS as u16, 12);
    assert!(live < 64 * wire + (1 << 20), "{wire} wire bytes caused {live} live bytes");
}

/// Largest accepted block (1536) with 2-byte payloads: memory follows the received bytes, not the block size.
#[test]
fn accepted_blocks_are_stored_at_real_length() {
    let mut a = VideoAssembler::new(3);
    let before = live_now();
    let pad = 1534u16.to_be_bytes();
    let mut wire = 0;
    for f in 0..MAX_PENDING_FRAMES as u16 {
        for i in 0..400u16 {
            a.push(&Unit {
                frame_id: 1000 + f,
                unit_index: i,
                total_units: 410,
                fec_units: 10,
                adaptive_index: 0,
                payload: &pad,
            });
            wire += 2 + 29;
        }
    }
    let live = (live_now() - before).max(0) as usize;
    println!("{wire} B on the wire -> {live} B live");
    assert!(live < 64 * wire, "{live}");
}
