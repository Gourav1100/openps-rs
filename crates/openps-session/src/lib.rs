//! Session runtime: owns the network link and drives the sans-IO [`Handshake`] and [`Stream`] on a dedicated
//! thread, so the UI/render threads only exchange [`Command`]s and [`Notice`]s with it.
//!
//! ```text
//!   reader thread --Datagram--\
//!                              >-- one queue --> runner thread (Handshake, then Stream) --> Notice queue (bounded)
//!   Handle::send(Command) ----/
//! ```
//!
//! Design points:
//! * One inbound queue carries datagrams and commands, so the runner blocks in exactly one place and wakes
//!   on whichever comes first or on the next engine timer. No polling loop, no busy waiting.
//! * Media delivery is bounded. If the consumer falls behind, new video/audio is dropped and counted rather
//!   than buffering without limit, and a dropped video frame triggers an IDR request (the decoder lost a
//!   reference). Lifecycle notices (`Connected`, `Failed`, `Closed`) always get through.
//! * The link is abstract ([`link::Link`]), so the whole stack runs against an in-memory console in tests.

pub mod link;
mod runner;

use openps_input::state::ControllerState;
use openps_stream::{Event, StreamConfig};
use openps_takion::handshake::HandshakeConfig;
use openps_takion::proto::StreamInfoPayload;
use openps_takion::senkusha::{SenkushaConfig, SenkushaResult};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

pub use link::Link;

/// Requests from the application to the running session.
#[derive(Debug)]
pub enum Command {
    /// Newest state of a pad (from the Con2PC mapper or a physical controller).
    Pad { pad: usize, state: ControllerState },
    /// A pad joined (`true`) or left (`false`); retried until the console acknowledges.
    PadConnected { pad: usize, connected: bool },
    /// Motion data from a physical pad.
    Sensors { pad: usize, gyro: [f32; 3], accel: [f32; 3], orientation: [f32; 4] },
    /// Ask the console for a keyframe (decoder lost sync).
    RequestIdr,
    /// End the session cleanly.
    Disconnect,
}

/// What the session tells the application.
#[derive(Debug)]
pub enum Notice {
    /// The Senkusha network test finished (`None` if it failed; the stream then uses default values).
    NetworkTested(Option<NetworkInfo>),
    Handshaking {
        attempt: u32,
    },
    Connected {
        takion_version: i32,
        stream_info: Box<StreamInfoPayload>,
    },
    Event(Event),
    /// The session could not start or died. Terminal.
    Failed(String),
    /// The session ended (we disconnected, or the console did). Terminal.
    Closed(Option<String>),
}

impl Notice {
    pub fn is_terminal(&self) -> bool {
        matches!(self, Notice::Failed(_) | Notice::Closed(_))
    }
}

/// What the pre-stream network test learned; the handshake configuration should use it (MTU for fragmentation and
/// the launch spec, RTT for the launch spec).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NetworkInfo {
    pub rtt_ms: i32,
    pub mtu_in: i32,
    pub mtu_out: i32,
}

impl Default for NetworkInfo {
    /// Values used when no test ran or it failed (the app's defaults: maximum MTU, RTT unknown).
    fn default() -> Self {
        Self { rtt_ms: 0, mtu_in: 1454, mtu_out: 1454 }
    }
}

impl From<&SenkushaResult> for NetworkInfo {
    fn from(r: &SenkushaResult) -> Self {
        Self { rtt_ms: r.rtt_ms, mtu_in: r.mtu_in, mtu_out: r.mtu_out }
    }
}

/// The optional Senkusha test: its own link (UDP port 9297 of the console) and settings.
pub struct NetworkTest {
    pub link: Link,
    pub config: SenkushaConfig,
    /// Pause between the test and the handshake (the app waits 2.5 s so the console settles).
    pub settle_s: f64,
}

pub struct ConnectParams {
    /// Builds a fresh handshake configuration per attempt (keys and session ciphers are single-use). It receives
    /// the network test result, or the defaults if there was none.
    pub handshake: Box<dyn FnMut(&NetworkInfo) -> HandshakeConfig + Send>,
    /// Run the Senkusha network test first.
    pub network_test: Option<NetworkTest>,
    pub stream: StreamConfig,
    /// Whole-handshake attempts before giving up (the app uses 3).
    pub attempts: u32,
    /// Pause between attempts in seconds (the app uses 0.15).
    pub attempt_delay_s: f64,
    /// Seconds without any datagram before a connected session is declared dead.
    pub idle_timeout_s: f64,
    /// Capacity of the notice queue (the app buffers 120 video frames).
    pub notice_capacity: usize,
}

impl ConnectParams {
    pub fn new(handshake: impl FnMut(&NetworkInfo) -> HandshakeConfig + Send + 'static, stream: StreamConfig) -> Self {
        Self {
            handshake: Box::new(handshake),
            network_test: None,
            stream,
            attempts: 3,
            attempt_delay_s: 0.15,
            idle_timeout_s: 10.0,
            notice_capacity: 256,
        }
    }
}

/// Counters since the session started.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub datagrams_in: u64,
    pub datagrams_out: u64,
    pub video_frames: u64,
    /// Video frames dropped because the application was not consuming notices fast enough.
    pub video_frames_dropped: u64,
    /// Audio/haptic/other events dropped for the same reason.
    pub other_events_dropped: u64,
    /// Inbound datagrams dropped because the runner was too far behind.
    pub datagrams_dropped: u64,
}

#[derive(Default)]
pub(crate) struct Shared {
    pub(crate) stop: AtomicBool,
    /// Notices queued but not yet taken by the application.
    pub(crate) queued: AtomicUsize,
    /// Datagrams/commands queued for the runner.
    pub(crate) inbound: AtomicUsize,
    pub(crate) datagrams_in: AtomicU64,
    pub(crate) datagrams_out: AtomicU64,
    pub(crate) video_frames: AtomicU64,
    pub(crate) video_frames_dropped: AtomicU64,
    pub(crate) other_events_dropped: AtomicU64,
    pub(crate) datagrams_dropped: AtomicU64,
}

pub(crate) enum Msg {
    Datagram(Vec<u8>),
    Cmd(Command),
    LinkClosed(String),
}

/// Application-side end of a running session.
pub struct Handle {
    tx: Sender<Msg>,
    rx: Receiver<Notice>,
    shared: Arc<Shared>,
    threads: Vec<JoinHandle<()>>,
}

/// A cloneable, thread-safe way to send commands to a running session (the [`Handle`] itself owns the notice
/// queue and so is used from one thread).
#[derive(Clone)]
pub struct Commander {
    tx: Sender<Msg>,
    shared: Arc<Shared>,
}

impl Commander {
    pub fn send(&self, cmd: Command) {
        self.shared.inbound.fetch_add(1, Ordering::Relaxed);
        if self.tx.send(Msg::Cmd(cmd)).is_err() {
            self.shared.inbound.fetch_sub(1, Ordering::Relaxed); // runner already gone
        }
    }

    pub fn set_pad(&self, pad: usize, state: ControllerState) {
        self.send(Command::Pad { pad, state });
    }

    pub fn request_idr(&self) {
        self.send(Command::RequestIdr);
    }

    pub fn disconnect(&self) {
        self.send(Command::Disconnect);
    }
}

impl Handle {
    /// A cloneable sender for commands, usable from any thread.
    pub fn commander(&self) -> Commander {
        Commander { tx: self.tx.clone(), shared: self.shared.clone() }
    }

    pub fn send(&self, cmd: Command) {
        self.commander().send(cmd);
    }

    pub fn set_pad(&self, pad: usize, state: ControllerState) {
        self.send(Command::Pad { pad, state });
    }

    pub fn request_idr(&self) {
        self.send(Command::RequestIdr);
    }

    pub fn disconnect(&self) {
        self.send(Command::Disconnect);
    }

    fn taken(&self, n: Notice) -> Notice {
        self.shared.queued.fetch_sub(1, Ordering::Relaxed);
        n
    }

    /// Next notice if one is ready.
    pub fn try_recv(&self) -> Option<Notice> {
        match self.rx.try_recv() {
            Ok(n) => Some(self.taken(n)),
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => None,
        }
    }

    /// Wait for the next notice. `None` on timeout or once the session thread has finished and the queue is empty.
    pub fn recv_timeout(&self, timeout: Duration) -> Option<Notice> {
        match self.rx.recv_timeout(timeout) {
            Ok(n) => Some(self.taken(n)),
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => None,
        }
    }

    pub fn stats(&self) -> Stats {
        let s = &self.shared;
        Stats {
            datagrams_in: s.datagrams_in.load(Ordering::Relaxed),
            datagrams_out: s.datagrams_out.load(Ordering::Relaxed),
            video_frames: s.video_frames.load(Ordering::Relaxed),
            video_frames_dropped: s.video_frames_dropped.load(Ordering::Relaxed),
            other_events_dropped: s.other_events_dropped.load(Ordering::Relaxed),
            datagrams_dropped: s.datagrams_dropped.load(Ordering::Relaxed),
        }
    }

    /// True once any of the session's threads has exited. After the terminal notice that is normal; *without* one it
    /// means a thread died (a bug), so no further notices will ever arrive and the caller must not keep waiting.
    pub fn has_stopped(&self) -> bool {
        self.threads.iter().any(|t| t.is_finished())
    }

    /// Ask the session to end and wait for its threads (at most ~100 ms: the reader wakes on its poll interval).
    pub fn join(mut self) {
        self.disconnect();
        self.shared.stop.store(true, Ordering::Relaxed);
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        // Best effort: end the session without blocking the caller. Threads exit on their own.
        let _ = self.tx.send(Msg::Cmd(Command::Disconnect));
        self.shared.stop.store(true, Ordering::Relaxed);
    }
}

/// Start a session on `link`. Returns immediately; progress arrives as [`Notice`]s.
pub fn connect(link: Link, params: ConnectParams) -> Handle {
    runner::spawn(link, params)
}
