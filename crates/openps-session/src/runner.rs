use crate::link::{Link, PacketSink, PacketSource};
use crate::{Command, ConnectParams, Handle, Msg, NetworkInfo, NetworkTest, Notice, Shared};
use openps_stream::{Event, Stream};
use openps_takion::handshake::{Handshake, Session};
use openps_takion::senkusha::{Senkusha, State as SenkushaState};
use std::sync::atomic::Ordering;
use std::sync::mpsc::{channel, sync_channel, Receiver, RecvTimeoutError, Sender, SyncSender};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Engine timers (retries 66/150 ms, history 100 ms, congestion 1 s) need a tick at least this often.
const TICK: Duration = Duration::from_millis(10);
/// The reader wakes this often to notice a stop request.
const READER_POLL: Duration = Duration::from_millis(100);
/// Notice-queue slots kept free for lifecycle notices so media can never starve them.
const LIFECYCLE_HEADROOM: usize = 8;
/// If the runner is this far behind, inbound datagrams are dropped (the console's FEC/IDR path recovers).
const MAX_INBOUND_BACKLOG: usize = 8192;

pub(crate) fn spawn(link: Link, params: ConnectParams) -> Handle {
    let shared = Arc::new(Shared::default());
    let (tx, rx) = channel::<Msg>();
    let (ntx, nrx) = sync_channel::<Notice>(params.notice_capacity.max(LIFECYCLE_HEADROOM + 1));
    let Link { sink, source } = link;

    let reader = {
        let (tx, shared) = (tx.clone(), shared.clone());
        std::thread::Builder::new()
            .name("openps-reader".into())
            .spawn(move || reader_loop(source, tx, shared))
            .expect("spawn reader")
    };
    let runner = {
        let shared = shared.clone();
        let capacity = params.notice_capacity.max(LIFECYCLE_HEADROOM + 1);
        std::thread::Builder::new()
            .name("openps-session".into())
            .spawn(move || {
                let mut r = Runner {
                    sink,
                    rx,
                    out: Emitter { tx: ntx, shared: shared.clone(), capacity },
                    shared,
                    start: Instant::now(),
                };
                r.run(params);
            })
            .expect("spawn runner")
    };
    Handle { tx, rx: nrx, shared, threads: vec![reader, runner] }
}

fn reader_loop(mut source: Box<dyn PacketSource>, tx: Sender<Msg>, shared: Arc<Shared>) {
    while !shared.stop.load(Ordering::Relaxed) {
        match source.recv(READER_POLL) {
            Ok(Some(d)) => {
                if shared.inbound.load(Ordering::Relaxed) >= MAX_INBOUND_BACKLOG {
                    shared.datagrams_dropped.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
                shared.inbound.fetch_add(1, Ordering::Relaxed);
                if tx.send(Msg::Datagram(d)).is_err() {
                    return; // runner gone
                }
            }
            Ok(None) => {}
            Err(e) => {
                let _ = tx.send(Msg::LinkClosed(e.to_string()));
                return;
            }
        }
    }
}

/// Delivers notices to the application without ever blocking on a slow consumer for media.
struct Emitter {
    tx: SyncSender<Notice>,
    shared: Arc<Shared>,
    capacity: usize,
}

impl Emitter {
    /// Lifecycle notices always get a slot (media leaves headroom). Returns false if the application is gone.
    fn lifecycle(&self, n: Notice) -> bool {
        self.shared.queued.fetch_add(1, Ordering::Relaxed);
        self.tx.send(n).is_ok()
    }

    /// Media: dropped (and counted) when the queue is nearly full. Returns false if the event was dropped.
    fn media(&self, e: Event) -> bool {
        let is_video = matches!(e, Event::Video(_));
        if self.shared.queued.load(Ordering::Relaxed) + LIFECYCLE_HEADROOM >= self.capacity {
            let counter = if is_video { &self.shared.video_frames_dropped } else { &self.shared.other_events_dropped };
            counter.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        self.shared.queued.fetch_add(1, Ordering::Relaxed);
        if self.tx.send(Notice::Event(e)).is_err() {
            self.shared.queued.fetch_sub(1, Ordering::Relaxed);
            return false;
        }
        if is_video {
            self.shared.video_frames.fetch_add(1, Ordering::Relaxed);
        }
        true
    }
}

struct Runner {
    sink: Box<dyn PacketSink>,
    rx: Receiver<Msg>,
    out: Emitter,
    shared: Arc<Shared>,
    start: Instant,
}

enum Exit {
    Closed(Option<String>),
    Failed(String),
}

impl Runner {
    fn now(&self) -> f64 {
        self.start.elapsed().as_secs_f64()
    }

    fn send_all(&mut self, packets: impl Iterator<Item = Vec<u8>>) -> Result<(), String> {
        for p in packets {
            self.sink.send(&p).map_err(|e| format!("send failed: {e}"))?;
            self.shared.datagrams_out.fetch_add(1, Ordering::Relaxed);
        }
        Ok(())
    }

    /// Next message, accounting for the inbound backlog counter.
    fn recv(&mut self, timeout: Duration) -> Result<Option<Msg>, ()> {
        match self.rx.recv_timeout(timeout) {
            Ok(m) => {
                self.shared.inbound.fetch_sub(1, Ordering::Relaxed);
                Ok(Some(m))
            }
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => Err(()),
        }
    }

    fn run(&mut self, mut params: ConnectParams) {
        let info = match params.network_test.take() {
            Some(nt) => match self.network_test(nt) {
                Ok(info) => info,
                Err(exit) => return self.finish(exit),
            },
            None => NetworkInfo::default(),
        };
        let exit = match self.handshake(&mut params, &info) {
            Ok(session) => self.stream(session, &params),
            Err(e) => e,
        };
        self.finish(exit);
    }

    fn finish(&mut self, exit: Exit) {
        match exit {
            Exit::Closed(reason) => self.out.lifecycle(Notice::Closed(reason)),
            Exit::Failed(why) => self.out.lifecycle(Notice::Failed(why)),
        };
    }

    /// Did the application ask us to stop while we were busy on another link?
    fn stop_requested(&mut self) -> Option<Exit> {
        while let Ok(m) = self.rx.try_recv() {
            self.shared.inbound.fetch_sub(1, Ordering::Relaxed);
            match m {
                Msg::Cmd(Command::Disconnect) => return Some(Exit::Closed(None)),
                Msg::LinkClosed(e) => return Some(Exit::Failed(format!("link closed: {e}"))),
                _ => {}
            }
        }
        None
    }

    // ---- network test ---------------------------------------------------------------------------------------------
    /// Run Senkusha on its own link. A failed test is not fatal: the defaults are used, as in the original.
    fn network_test(&mut self, mut nt: NetworkTest) -> Result<NetworkInfo, Exit> {
        let seed =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(1);
        let mut x = seed | 1;
        let mut test = Senkusha::new(nt.config.clone(), move || {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x
        });
        let mut link_ok = true;
        while matches!(test.state(), SenkushaState::Running) && link_ok {
            if let Some(exit) = self.stop_requested() {
                return Err(exit);
            }
            let now = self.now();
            test.handle_timeout(now);
            while let Some(p) = test.poll_transmit(now) {
                if nt.link.sink.send(&p).is_err() {
                    link_ok = false;
                }
            }
            if !matches!(test.state(), SenkushaState::Running) {
                break;
            }
            let wait = test.poll_timeout().map(|t| (t - self.now()).clamp(0.001, 0.05)).unwrap_or(0.05);
            match nt.link.source.recv(Duration::from_secs_f64(wait)) {
                Ok(Some(d)) => test.handle_datagram(self.now(), &d),
                Ok(None) => {}
                Err(_) => link_ok = false,
            }
        }
        let info = test.result().map(NetworkInfo::from);
        if !self.out.lifecycle(Notice::NetworkTested(info)) {
            return Err(Exit::Closed(None));
        }
        // let the console settle, then discard anything left over from the test before the real handshake
        let until = self.now() + nt.settle_s;
        while self.now() < until {
            if let Some(exit) = self.stop_requested() {
                return Err(exit);
            }
            std::thread::sleep(Duration::from_secs_f64((until - self.now()).clamp(0.0, 0.02)));
        }
        for _ in 0..64 {
            if !matches!(nt.link.source.recv(Duration::from_millis(1)), Ok(Some(_))) {
                break;
            }
        }
        Ok(info.unwrap_or_default())
    }

    // ---- handshake ------------------------------------------------------------------------------------------
    fn handshake(&mut self, params: &mut ConnectParams, info: &NetworkInfo) -> Result<Session, Exit> {
        let attempts = params.attempts.max(1);
        let mut last_err = String::from("handshake failed");
        for attempt in 1..=attempts {
            if !self.out.lifecycle(Notice::Handshaking { attempt }) {
                return Err(Exit::Closed(None));
            }
            match self.handshake_once(Handshake::new((params.handshake)(info)))? {
                Ok(session) => return Ok(session),
                Err(why) => last_err = why,
            }
            if attempt < attempts {
                // pause between attempts, but stay responsive to a Disconnect
                let until = self.now() + params.attempt_delay_s;
                while self.now() < until {
                    let wait = Duration::from_secs_f64((until - self.now()).max(0.001));
                    match self.recv(wait) {
                        Ok(Some(Msg::Cmd(Command::Disconnect))) | Err(()) => return Err(Exit::Closed(None)),
                        Ok(Some(Msg::LinkClosed(e))) => return Err(Exit::Failed(format!("link closed: {e}"))),
                        _ => {}
                    }
                }
            }
        }
        Err(Exit::Failed(format!("could not connect after {attempts} attempts: {last_err}")))
    }

    /// One handshake attempt. Outer error = the session must end; inner error = this attempt failed.
    fn handshake_once(&mut self, mut hs: Handshake) -> Result<Result<Session, String>, Exit> {
        loop {
            let now = self.now();
            hs.handle_timeout(now);
            let mut pending = vec![];
            while let Some(p) = hs.poll_transmit(now) {
                pending.push(p);
            }
            self.send_all(pending.into_iter()).map_err(Exit::Failed)?;
            if hs.is_complete() {
                return Ok(hs.into_session().ok_or_else(|| "handshake completed without a session".to_string()));
            }
            if let Some(why) = hs.failed() {
                return Ok(Err(why.to_string()));
            }
            let wait = hs.poll_timeout().map(|t| (t - self.now()).clamp(0.001, 0.25)).unwrap_or(0.05);
            match self.recv(Duration::from_secs_f64(wait)) {
                Ok(Some(Msg::Datagram(d))) => {
                    self.shared.datagrams_in.fetch_add(1, Ordering::Relaxed);
                    hs.handle_datagram(self.now(), &d);
                }
                Ok(Some(Msg::Cmd(Command::Disconnect))) | Err(()) => return Err(Exit::Closed(None)),
                Ok(Some(Msg::Cmd(_))) => {} // pad input before the stream exists is dropped
                Ok(Some(Msg::LinkClosed(e))) => return Err(Exit::Failed(format!("link closed: {e}"))),
                Ok(None) => {}
            }
        }
    }

    // ---- streaming -------------------------------------------------------------------------------------------
    fn stream(&mut self, session: Session, params: &ConnectParams) -> Exit {
        let version = session.takion_version;
        let info = Box::new(session.stream_info.clone());
        let mut stream = Stream::new(session, params.stream.clone());
        if !self.out.lifecycle(Notice::Connected { takion_version: version, stream_info: info }) {
            return Exit::Closed(None);
        }
        let mut last_rx = self.now();
        let mut need_idr = false;
        loop {
            let now = self.now();
            if let Err(e) = self.flush(&mut stream) {
                return Exit::Failed(e);
            }
            if now - last_rx > params.idle_timeout_s {
                return Exit::Failed(format!("console stopped responding for {:.0} s", params.idle_timeout_s));
            }
            match self.recv(TICK) {
                Ok(Some(Msg::Datagram(d))) => {
                    let now = self.now();
                    last_rx = now;
                    self.shared.datagrams_in.fetch_add(1, Ordering::Relaxed);
                    for ev in stream.handle_datagram(now, &d) {
                        if let Event::Disconnected(reason) = &ev {
                            let reason = reason.clone();
                            self.out.media(ev);
                            let _ = self.flush(&mut stream);
                            return Exit::Closed(reason);
                        }
                        let is_video = matches!(ev, Event::Video(_));
                        if !self.out.media(ev) && is_video {
                            need_idr = true; // a dropped frame breaks the reference chain
                        }
                    }
                }
                Ok(Some(Msg::Cmd(cmd))) => {
                    let now = self.now();
                    match cmd {
                        Command::Pad { pad, state } => stream.update_pad(now, pad, &state),
                        Command::PadConnected { pad, connected } => stream.set_pad_connected(now, pad, connected),
                        Command::Sensors { pad, gyro, accel, orientation } => {
                            stream.update_sensors(pad, gyro, accel, orientation)
                        }
                        Command::RequestIdr => stream.request_idr(now),
                        Command::Disconnect => {
                            stream.disconnect();
                            let _ = self.flush(&mut stream);
                            return Exit::Closed(None);
                        }
                    }
                }
                Ok(Some(Msg::LinkClosed(e))) => return Exit::Failed(format!("link closed: {e}")),
                Ok(None) => {}
                Err(()) => return Exit::Closed(None), // every handle dropped
            }
            let now = self.now();
            stream.tick(now);
            if std::mem::take(&mut need_idr) {
                stream.request_idr(now);
            }
        }
    }

    fn flush(&mut self, stream: &mut Stream) -> Result<(), String> {
        let packets: Vec<Vec<u8>> = std::iter::from_fn(|| stream.poll_transmit()).collect();
        self.send_all(packets.into_iter())
    }
}
