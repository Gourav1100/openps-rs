//! The TCP control channel to the console, as a trait so the core can be tested without a console, plus the
//! background worker that keeps it alive and reports what the console says.

use crate::events::{AppEvent, EventSink};
use openps_crypto::RpCipher;
use openps_rp::client::ControlSession;
use openps_rp::ctrl::Inbound;
use openps_rp::session::CtrlInfo;
use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ControlCommand {
    PadJoin { pad: u8, controller_type: u8 },
    PadLeave { pad: u8 },
    Pin(String),
    MicRequest,
    MicStatus { muted: bool },
    RestMode,
}

pub trait ControlLink: Send {
    fn version(&self) -> String;
    fn session_id(&self) -> String;
    /// The session cipher: its counter-0 keystream encrypts the Takion launch spec.
    fn cipher(&self) -> RpCipher;
    fn info(&self) -> CtrlInfo;
    /// Messages that arrived while the session id was awaited.
    fn take_pending(&mut self) -> Vec<Inbound>;
    /// Wait up to `timeout` for console messages. Keepalives are answered internally.
    fn poll(&mut self, timeout: Duration) -> io::Result<Vec<Inbound>>;
    fn send(&mut self, cmd: ControlCommand) -> io::Result<()>;
}

/// Production control link over the real TCP session.
pub struct RealControl {
    s: ControlSession,
    session_id: String,
}

impl RealControl {
    pub fn new(s: ControlSession, session_id: String) -> Self {
        Self { s, session_id }
    }
}

impl ControlLink for RealControl {
    fn version(&self) -> String {
        self.s.version.clone()
    }
    fn session_id(&self) -> String {
        self.session_id.clone()
    }
    fn cipher(&self) -> RpCipher {
        self.s.cipher.clone()
    }
    fn info(&self) -> CtrlInfo {
        CtrlInfo {
            server_type: self.s.info.server_type,
            full_hd: self.s.info.full_hd,
            prohibited: self.s.info.prohibited,
        }
    }
    fn take_pending(&mut self) -> Vec<Inbound> {
        std::mem::take(&mut self.s.pending)
    }

    fn poll(&mut self, timeout: Duration) -> io::Result<Vec<Inbound>> {
        let mut out = vec![];
        let drain = |s: &mut ControlSession, out: &mut Vec<Inbound>| -> io::Result<()> {
            while let Some(m) =
                s.rx.next_message().map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("{e:?}")))?
            {
                if m == Inbound::Keepalive {
                    s.stream.write_all(&openps_rp::ctrl::KEEPALIVE_REPLY)?;
                } else {
                    out.push(m);
                }
            }
            Ok(())
        };
        drain(&mut self.s, &mut out)?;
        if !out.is_empty() {
            return Ok(out);
        }
        self.s.stream.set_read_timeout(Some(timeout.max(Duration::from_millis(1))))?;
        let mut buf = [0u8; 2048];
        match self.s.stream.read(&mut buf) {
            Ok(0) => return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "console closed the control channel")),
            Ok(n) => self.s.rx.feed(&buf[..n]),
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {}
            Err(e) => return Err(e),
        }
        drain(&mut self.s, &mut out)?;
        Ok(out)
    }

    fn send(&mut self, cmd: ControlCommand) -> io::Result<()> {
        self.s.send(|tx| match cmd {
            ControlCommand::PadJoin { pad, controller_type } => tx.pad_join(pad, controller_type),
            ControlCommand::PadLeave { pad } => tx.pad_leave(pad),
            ControlCommand::Pin(p) => tx.pin(&p),
            ControlCommand::MicRequest => tx.mic_request(),
            ControlCommand::MicStatus { muted } => tx.mic_status(muted),
            ControlCommand::RestMode => tx.rest_mode(),
        })
    }
}

/// Short label and detail for the UI.
pub fn describe(m: &Inbound) -> (String, String) {
    match m {
        Inbound::DisplayStatus { can_display } => ("display_status".into(), can_display.to_string()),
        Inbound::PinRequired => ("pin_required".into(), String::new()),
        Inbound::PinResult(r) => ("pin_result".into(), r.to_string()),
        Inbound::PadDropped(p) => ("pad_dropped".into(), p.to_string()),
        Inbound::Streamable(s) => ("streamable".into(), s.to_string()),
        Inbound::SessionId(_) => ("session_id".into(), String::new()), // the id itself is not shown
        Inbound::PadJoinResult { pad, accepted } => ("pad_join".into(), format!("{pad}:{accepted}")),
        Inbound::PadLeaveResult { pad, accepted } => ("pad_leave".into(), format!("{pad}:{accepted}")),
        Inbound::PasscodeRequest(p) => ("passcode_request".into(), p.to_string()),
        Inbound::UserJoinResult { pad, status } => ("user_join".into(), format!("{pad}:{status}")),
        Inbound::AccountUnknown => ("account_unknown".into(), String::new()),
        Inbound::Keepalive => ("keepalive".into(), String::new()),
        Inbound::Unknown { ty, len } => ("unknown".into(), format!("type {ty}, {len} bytes")),
    }
}

/// Runs until `stop` is set or the console closes the channel: forwards messages as events and sends queued
/// commands. Returns the error that ended it, if any.
pub fn run_worker(
    mut link: Box<dyn ControlLink>,
    commands: Receiver<ControlCommand>,
    stop: Arc<AtomicBool>,
    events: Arc<dyn EventSink>,
) -> Option<io::Error> {
    for m in link.take_pending() {
        let (kind, detail) = describe(&m);
        events.emit(AppEvent::Control { kind, detail });
    }
    while !stop.load(Ordering::Relaxed) {
        loop {
            match commands.recv_timeout(Duration::from_millis(1)) {
                Ok(c) => {
                    if let Err(e) = link.send(c) {
                        return Some(e);
                    }
                }
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => return None,
            }
        }
        match link.poll(Duration::from_millis(100)) {
            Ok(msgs) => {
                for m in msgs {
                    let (kind, detail) = describe(&m);
                    events.emit(AppEvent::Control { kind, detail });
                }
            }
            Err(e) => return Some(e),
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::VecSink;
    use std::collections::VecDeque;
    use std::sync::mpsc::channel;
    use std::sync::Mutex;

    struct Fake {
        inbox: Arc<Mutex<VecDeque<Inbound>>>,
        sent: Arc<Mutex<Vec<ControlCommand>>>,
        fail_after: Option<usize>,
    }

    impl ControlLink for Fake {
        fn version(&self) -> String {
            "1.0".into()
        }
        fn session_id(&self) -> String {
            "s".into()
        }
        fn cipher(&self) -> RpCipher {
            RpCipher::new([0; 16], [0; 16], [0; 16])
        }
        fn info(&self) -> CtrlInfo {
            CtrlInfo { server_type: None, full_hd: true, prohibited: false }
        }
        fn take_pending(&mut self) -> Vec<Inbound> {
            vec![Inbound::DisplayStatus { can_display: true }]
        }
        fn poll(&mut self, timeout: Duration) -> io::Result<Vec<Inbound>> {
            std::thread::sleep(timeout.min(Duration::from_millis(5)));
            Ok(self.inbox.lock().unwrap().drain(..).collect())
        }
        fn send(&mut self, cmd: ControlCommand) -> io::Result<()> {
            let mut s = self.sent.lock().unwrap();
            if self.fail_after.is_some_and(|n| s.len() >= n) {
                return Err(io::Error::new(io::ErrorKind::BrokenPipe, "gone"));
            }
            s.push(cmd);
            Ok(())
        }
    }

    #[test]
    fn worker_forwards_messages_and_commands_and_stops_on_request() {
        let inbox = Arc::new(Mutex::new(VecDeque::new()));
        let sent = Arc::new(Mutex::new(vec![]));
        let (tx, rx) = channel();
        let stop = Arc::new(AtomicBool::new(false));
        let sink = Arc::new(VecSink::default());
        let (i2, s2, st2, sk2) = (inbox.clone(), sent.clone(), stop.clone(), sink.clone());
        let t = std::thread::spawn(move || {
            run_worker(Box::new(Fake { inbox: i2, sent: s2, fail_after: None }), rx, st2, sk2)
        });
        inbox.lock().unwrap().push_back(Inbound::PadJoinResult { pad: 1, accepted: true });
        tx.send(ControlCommand::PadJoin { pad: 1, controller_type: 6 }).unwrap();
        std::thread::sleep(Duration::from_millis(80));
        stop.store(true, Ordering::Relaxed);
        assert!(t.join().unwrap().is_none());
        assert_eq!(sent.lock().unwrap().as_slice(), &[ControlCommand::PadJoin { pad: 1, controller_type: 6 }]);
        let kinds: Vec<String> = sink
            .events()
            .into_iter()
            .filter_map(|e| if let AppEvent::Control { kind, .. } = e { Some(kind) } else { None })
            .collect();
        assert_eq!(kinds, vec!["display_status", "pad_join"]); // pending first, then live
    }

    #[test]
    fn worker_reports_a_send_error_and_a_dropped_command_channel_ends_it_quietly() {
        let (tx, rx) = channel();
        let sent = Arc::new(Mutex::new(vec![]));
        let link = Fake { inbox: Default::default(), sent, fail_after: Some(0) };
        tx.send(ControlCommand::RestMode).unwrap();
        let err = run_worker(Box::new(link), rx, Arc::new(AtomicBool::new(false)), Arc::new(VecSink::default()));
        assert_eq!(err.unwrap().kind(), io::ErrorKind::BrokenPipe);
        let (tx, rx) = channel::<ControlCommand>();
        drop(tx);
        let link = Fake { inbox: Default::default(), sent: Default::default(), fail_after: None };
        assert!(
            run_worker(Box::new(link), rx, Arc::new(AtomicBool::new(false)), Arc::new(VecSink::default())).is_none()
        );
    }

    #[test]
    fn descriptions_never_reveal_the_session_id() {
        assert_eq!(describe(&Inbound::SessionId("SECRET".into())), ("session_id".into(), String::new()));
        assert_eq!(
            describe(&Inbound::PadJoinResult { pad: 2, accepted: false }),
            ("pad_join".into(), "2:false".into())
        );
    }
}
