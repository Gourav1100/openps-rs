//! The PSN session messages of remote registration (09b section 1): what we POST and what arrives on the push WebSocket.
//! JSON bodies are built as text in the exact shapes the original sends (the console's parser is what it is), parsed with serde.

use serde::{Deserialize, Serialize};

pub const CREATE_SESSION_URL: &str = "https://web.np.playstation.com/api/sessionManager/v1/remotePlaySessions";
pub const COMMANDS_URL: &str = "https://web.np.playstation.com/api/cloudAssistedNavigation/v2/users/me/commands";
pub const PUSH_SERVER_URL: &str = "https://mobile-pushcl.np.communication.playstation.net/np/serveraddr?version=2.1&fields=keepAliveStatus&keepAliveStatusType=3";
pub const PAYLOAD_PREFIX: &str = "ver=1.0, type=text, body=";
const SKEY: &str = "AAAAAAAAAAAAAAAAAAAAAA==";

/// A PSN session id as we accept it: 1..=128 characters of `A-Za-z0-9_-` (anything else could break out of a JSON string or a URL path).
pub fn valid_session_id(id: &str) -> bool {
    (1..=128).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// JSON-escape `s` (without the surrounding quotes).
fn esc(s: &str) -> String {
    let q = serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into());
    q.get(1..q.len().saturating_sub(1)).unwrap_or("").to_string()
}

/// Percent-encode everything but the unreserved characters, so a value can only ever be one path segment.
fn path_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub fn session_message_url(session_id: &str) -> String {
    format!("{CREATE_SESSION_URL}/{}/sessionMessage", path_segment(session_id))
}
pub fn leave_session_url(session_id: &str) -> String {
    format!("{CREATE_SESSION_URL}/{}/members/me", path_segment(session_id))
}

/// Domains the push server may live in (the bearer token is sent to it).
const PUSH_DOMAINS: [&str; 2] = [".playstation.net", ".playstation.com"];

/// The WebSocket URL for a push server `fqdn` (only a plain host name below a Sony domain is accepted: it comes from the network).
pub fn websocket_url(fqdn: &str) -> Option<String> {
    let lower = fqdn.to_ascii_lowercase();
    if !PUSH_DOMAINS.iter().any(|d| lower.len() > d.len() && lower.ends_with(d)) {
        return None;
    }
    let label = |l: &str| {
        (1..=63).contains(&l.len())
            && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            && !l.starts_with('-')
            && !l.ends_with('-')
    };
    let ok = !fqdn.is_empty() && fqdn.len() <= 253 && fqdn.split('.').all(label);
    ok.then(|| format!("wss://{fqdn}/np/pushNotification"))
}

/// Headers of the push WebSocket (besides `Authorization` and `Host`).
pub const WS_HEADERS: [(&str, &str); 7] = [
    ("Sec-WebSocket-Protocol", "np-pushpacket"),
    ("User-Agent", "WebSocket++/0.8.2"),
    ("X-PSN-APP-TYPE", "REMOTE_PLAY"),
    ("X-PSN-APP-VER", "RemotePlay/1.0"),
    ("X-PSN-KEEP-ALIVE-STATUS-TYPE", "3"),
    ("X-PSN-OS-VER", "Windows/10.0"),
    ("X-PSN-PROTOCOL-VERSION", "2.1"),
];

/// `POST .../remotePlaySessions`: creates the session, naming our push context.
pub fn create_session_body(context_id: &str) -> String {
    format!(
        "{{\"remotePlaySessions\":[{{\"members\":[{{\"accountId\":\"me\",\"deviceUniqueId\":\"me\",\"platform\":\"me\",\"pushContexts\":[{{\"pushContextId\":\"{context_id}\"}}]}}]}}]}}"
    )
}

/// `POST .../commands`: asks the console (through PSN) to join the session. `data1`/`data2` are our two secrets, base64.
pub fn join_command_body(
    duid: &str,
    account_id: u64,
    session_id: &str,
    secret_i: &[u8; 16],
    secret_j: &[u8; 16],
) -> String {
    use base64::Engine as _;
    let b64 = base64::engine::general_purpose::STANDARD;
    let inner = format!(
        "{{\"accountId\":{account_id},\"roomId\":0,\"sessionId\":\"{}\",\"clientType\":\"Windows\",\"data1\":\"{}\",\"data2\":\"{}\"}}",
        esc(session_id),
        b64.encode(secret_i),
        b64.encode(secret_j)
    );
    let outer = serde_json::to_string(&inner).unwrap_or_else(|_| "\"\"".into());
    format!(
        "{{\"commandDetail\":{{\"commandType\":\"remotePlay\",\"duid\":\"{}\",\"messageDestination\":\"SQS\",\"parameters\":{{\"initialParams\":{outer}}},\"platform\":\"PS5\"}}}}\r\n",
        esc(duid)
    )
}

// ---- incoming ---------------------------------------------------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
pub struct WsMessage {
    #[serde(rename = "dataType")]
    pub data_type: Option<String>,
    pub body: Option<WsBody>,
    pub contexts: Option<Vec<WsContext>>,
}
#[derive(Debug, Default, Deserialize)]
pub struct WsBody {
    pub data: Option<WsData>,
}
#[derive(Debug, Default, Deserialize)]
pub struct WsData {
    #[serde(rename = "sessionId")]
    pub session_id: Option<String>,
    #[serde(rename = "customData1")]
    pub custom_data1: Option<String>,
    #[serde(rename = "sessionMessage")]
    pub session_message: Option<WsSessionMessage>,
}
#[derive(Debug, Default, Deserialize)]
pub struct WsSessionMessage {
    pub payload: Option<String>,
}
#[derive(Debug, Default, Deserialize)]
pub struct WsContext {
    #[serde(rename = "contextId")]
    pub context_id: Option<String>,
}

/// Notification names we react to.
pub const N_CREATED: &str = "psn:sessionManager:sys:remotePlaySession:created";
pub const N_CUSTOM_DATA: &str = "psn:sessionManager:sys:rps:customData1:updated";
pub const N_MESSAGE: &str = "psn:sessionManager:sys:rps:sessionMessage:created";
pub const N_SESSION_DELETED: &str = "psn:sessionManager:sys:remotePlaySession:deleted";
pub const N_MEMBER_DELETED: &str = "psn:sessionManager:sys:rps:members:deleted";

impl WsMessage {
    pub fn parse(text: &str) -> Option<Self> {
        lenient_json(text)
    }
    pub fn data_type(&self) -> &str {
        self.data_type.as_deref().unwrap_or("")
    }
    fn data(&self) -> Option<&WsData> {
        self.body.as_ref()?.data.as_ref()
    }
    pub fn session_id(&self) -> &str {
        self.data().and_then(|d| d.session_id.as_deref()).unwrap_or("")
    }
    pub fn custom_data1(&self) -> &str {
        self.data().and_then(|d| d.custom_data1.as_deref()).unwrap_or("")
    }
    pub fn payload(&self) -> Option<&str> {
        self.data()?.session_message.as_ref()?.payload.as_deref()
    }

    /// The original's intake filter: a session id must be present; once we know ours it must match; and the message must carry
    /// our push context (or none at all).
    pub fn is_for_us(&self, our_session: &str, our_context: &str) -> bool {
        let sid = self.session_id();
        if self.data().and_then(|d| d.session_id.as_ref()).is_none() || !valid_session_id(sid) {
            return false;
        }
        if !our_session.is_empty() && our_session != sid {
            return false;
        }
        match &self.contexts {
            Some(c) if !c.is_empty() => c.iter().any(|x| x.context_id.as_deref() == Some(our_context)),
            _ => true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Candidate {
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub addr: String,
    #[serde(rename = "mappedAddr", default)]
    pub mapped_addr: String,
    #[serde(default)]
    pub port: i64,
    #[serde(rename = "mappedPort", default)]
    pub mapped_port: i64,
}

#[derive(Debug, Default, Deserialize)]
pub struct ConnRequest {
    pub sid: Option<i64>,
    #[serde(rename = "natType")]
    pub nat_type: Option<i64>,
    #[serde(default)]
    pub candidate: Vec<Candidate>,
    #[serde(rename = "localHashedId")]
    pub local_hashed_id: Option<String>,
}

/// The JSON after `body=` in a session message.
#[derive(Debug, Default, Deserialize)]
pub struct EndpointPayload {
    pub action: Option<String>,
    #[serde(rename = "reqId")]
    pub req_id: Option<i64>,
    pub error: Option<i64>,
    #[serde(rename = "connRequest")]
    pub conn_request: Option<ConnRequest>,
}

/// Parse JSON the way Gson's lenient `fromJson` does, for the cases that matter here: surrounding junk (whitespace, NULs, a line break),
/// anything after the first value, and a value left out entirely (`"x":,` or `"x":}`, which the original itself sends in its ACCEPT).
fn lenient_json<T: serde::de::DeserializeOwned>(text: &str) -> Option<T> {
    let text = text.trim_matches(|c: char| c.is_whitespace() || c.is_control());
    let first = |s: &str| serde_json::Deserializer::from_str(s).into_iter::<T>().next()?.ok();
    first(text).or_else(|| {
        // a missing value becomes null (outside of strings only)
        let mut out = String::with_capacity(text.len() + 8);
        let (mut in_str, mut esc, mut prev) = (false, false, ' ');
        for c in text.chars() {
            if !in_str && prev == ':' && matches!(c, ',' | '}' | ']') {
                out.push_str("null");
            }
            out.push(c);
            if in_str {
                if esc {
                    esc = false;
                } else if c == '\\' {
                    esc = true;
                } else if c == '"' {
                    in_str = false;
                }
            } else if c == '"' {
                in_str = true;
            }
            if !c.is_whitespace() {
                prev = c;
            }
        }
        first(&out)
    })
}

impl EndpointPayload {
    /// The original removes the prefix wherever it occurs, then parses the rest as (lenient) JSON.
    pub fn parse(payload: &str) -> Option<Self> {
        lenient_json(&payload.replace(PAYLOAD_PREFIX, ""))
    }
    pub fn action(&self) -> &str {
        self.action.as_deref().unwrap_or("")
    }
}

// ---- outgoing session messages ------------------------------------------------------------------------------------------------------

/// A LOCAL candidate: one of our addresses with our UDP port.
pub fn local_candidate(ip: &str, port: u16) -> String {
    format!(
        "{{\"type\":\"LOCAL\",\"addr\":\"{}\",\"mappedAddr\":\"0.0.0.0\",\"port\":{port},\"mappedPort\":0}}",
        esc(ip)
    )
}
/// A STATIC candidate: a public address with our UDP port (not a mapped port).
pub fn static_candidate(ip: &str, port: u16) -> String {
    format!(
        "{{\"type\":\"STATIC\",\"addr\":\"{}\",\"mappedAddr\":\"0.0.0.0\",\"port\":{port},\"mappedPort\":0}}",
        esc(ip)
    )
}

/// OFFER (reqId 1 in registration): our candidates and the id the console puts in its hole-punch packets.
pub fn offer_payload(
    req_id: i64,
    sid: u16,
    nat_type: i64,
    candidates: &[String],
    account_id: u64,
    local_hashed_id: &str,
) -> String {
    format!(
        "{PAYLOAD_PREFIX}{{\"action\":\"OFFER\",\"reqId\":{req_id},\"error\":0,\"connRequest\":{{\"sid\":{sid},\"peerSid\":0,\"skey\":\"{SKEY}\",\"natType\":{nat_type},\"candidate\":[{}],\"defaultRouteMacAddr\":\"\",\"localPeerAddr\":{{\"accountId\":\"{account_id}\",\"platform\":\"REMOTE_PLAY\"}},\"localHashedId\":\"{}\"}}}}",
        candidates.join(","),
        esc(local_hashed_id)
    )
}

/// What the hole punch found: the console endpoint that answered, and our address as it saw it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PunchResult {
    pub kind: String,
    pub console_ip: String,
    pub console_port: u16,
    pub mapped_ip: String,
    pub mapped_port: u16,
}

/// How the ACCEPT writes `localPeerAddr`. The original sends the literal `"localPeerAddr":,` (no value: not valid JSON) and the console
/// accepts it; that is the default. `Valid` writes the same object as the OFFER, in case a console is strict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AcceptStyle {
    #[default]
    Original,
    Valid,
}

/// ACCEPT (reqId 2): sent after the hole punch worked.
pub fn accept_payload(
    req_id: i64,
    sid: u16,
    peer_sid: i64,
    nat_type: i64,
    p: &PunchResult,
    account_id: u64,
    style: AcceptStyle,
) -> String {
    let local_peer = match style {
        AcceptStyle::Original => String::new(),
        AcceptStyle::Valid => format!("{{\"accountId\":\"{account_id}\",\"platform\":\"REMOTE_PLAY\"}}"),
    };
    format!(
        "{PAYLOAD_PREFIX}{{\"action\":\"ACCEPT\",\"reqId\":{req_id},\"error\":0,\"connRequest\":{{\"sid\":{sid},\"peerSid\":{peer_sid},\"skey\":\"{SKEY}\",\"natType\":{nat_type},\"candidate\":[{{\"type\":\"{}\",\"addr\":\"{}\",\"mappedAddr\":\"{}\",\"port\":{},\"mappedPort\":{}}}],\"defaultRouteMacAddr\":\"\",\"localPeerAddr\":{local_peer},\"localHashedId\":\"\"}}}}",
        esc(&p.kind),
        esc(&p.console_ip),
        esc(&p.mapped_ip),
        p.console_port,
        p.mapped_port
    )
}

/// RESULT: acknowledges an incoming OFFER or ACCEPT, echoing its `reqId`.
pub fn result_payload(req_id: i64) -> String {
    format!("{PAYLOAD_PREFIX}{{\"action\":\"RESULT\",\"reqId\":{req_id},\"error\":0,\"connRequest\":{{}}}}")
}

/// The body POSTed to `.../sessionMessage`: the payload wrapped for the console (`to`).
pub fn session_message_body(payload: &str, account_id: u64, duid: &str) -> String {
    #[derive(Serialize)]
    struct To<'a> {
        #[serde(rename = "accountId")]
        account_id: String,
        #[serde(rename = "deviceUniqueId")]
        duid: &'a str,
        platform: &'a str,
    }
    #[derive(Serialize)]
    struct Msg<'a> {
        channel: &'a str,
        payload: &'a str,
        to: Vec<To<'a>>,
    }
    serde_json::to_string(&Msg {
        channel: "remote_play:1",
        payload,
        to: vec![To { account_id: account_id.to_string(), duid, platform: "PS5" }],
    })
    .expect("serialisable")
}

/// A copy of a message that is safe to put in a log: dotted addresses, long digit runs (account ids, MACs) and long tokens (ids, keys) are
/// replaced, the structure and field names stay. At most 800 characters.
pub fn redact(text: &str) -> String {
    let mut out = String::new();
    let chars: Vec<char> = text.chars().take(800).collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_ascii_alphanumeric()
            || matches!(c, '.' | '+' | '/' | '=' | ':') && i > 0 && chars[i - 1].is_ascii_alphanumeric()
        {
            let start = i;
            while i < chars.len()
                && (chars[i].is_ascii_alphanumeric() || matches!(chars[i], '.' | '+' | '/' | '=' | ':' | '-' | '_'))
            {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            let dotted = word.split('.').count() == 4
                && word.split('.').all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
            let digits = word.bytes().filter(u8::is_ascii_digit).count();
            out.push_str(&if dotted || word.len() >= 12 || digits >= 7 { "<x>".to_string() } else { word });
        } else {
            out.push(c);
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_session_and_join_bodies_are_exactly_the_originals() {
        assert_eq!(
            create_session_body("ctx-1"),
            r#"{"remotePlaySessions":[{"members":[{"accountId":"me","deviceUniqueId":"me","platform":"me","pushContexts":[{"pushContextId":"ctx-1"}]}]}]}"#
        );
        let body =
            join_command_body("00000007aa", 4382906891241957369, "sess-9", b"ABCDEFGHIJKLMNOP", b"QRSTUVWXYZ234567");
        assert_eq!(
            body,
            "{\"commandDetail\":{\"commandType\":\"remotePlay\",\"duid\":\"00000007aa\",\"messageDestination\":\"SQS\",\"parameters\":{\"initialParams\":\"{\\\"accountId\\\":4382906891241957369,\\\"roomId\\\":0,\\\"sessionId\\\":\\\"sess-9\\\",\\\"clientType\\\":\\\"Windows\\\",\\\"data1\\\":\\\"QUJDREVGR0hJSktMTU5PUA==\\\",\\\"data2\\\":\\\"UVJTVFVWV1hZWjIzNDU2Nw==\\\"}\"},\"platform\":\"PS5\"}}\r\n"
        );
        // it is valid JSON, and initialParams is itself JSON with the account id as a number
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        let inner: serde_json::Value =
            serde_json::from_str(v["commandDetail"]["parameters"]["initialParams"].as_str().unwrap()).unwrap();
        assert_eq!(inner["accountId"].as_u64(), Some(4382906891241957369));
        assert_eq!(inner["sessionId"], "sess-9");
    }

    #[test]
    fn offer_accept_result_use_the_original_templates() {
        let cands = vec![local_candidate("192.168.1.10", 40000), static_candidate("203.0.113.5", 40000)];
        let offer = offer_payload(1, 5123, 0, &cands, 1234, "aGFzaA==");
        assert_eq!(
            offer,
            "ver=1.0, type=text, body={\"action\":\"OFFER\",\"reqId\":1,\"error\":0,\"connRequest\":{\"sid\":5123,\"peerSid\":0,\"skey\":\"AAAAAAAAAAAAAAAAAAAAAA==\",\"natType\":0,\"candidate\":[{\"type\":\"LOCAL\",\"addr\":\"192.168.1.10\",\"mappedAddr\":\"0.0.0.0\",\"port\":40000,\"mappedPort\":0},{\"type\":\"STATIC\",\"addr\":\"203.0.113.5\",\"mappedAddr\":\"0.0.0.0\",\"port\":40000,\"mappedPort\":0}],\"defaultRouteMacAddr\":\"\",\"localPeerAddr\":{\"accountId\":\"1234\",\"platform\":\"REMOTE_PLAY\"},\"localHashedId\":\"aGFzaA==\"}}"
        );
        assert!(serde_json::from_str::<serde_json::Value>(offer.strip_prefix(PAYLOAD_PREFIX).unwrap()).is_ok());
        assert_eq!(
            result_payload(7),
            "ver=1.0, type=text, body={\"action\":\"RESULT\",\"reqId\":7,\"error\":0,\"connRequest\":{}}"
        );
        let p = PunchResult {
            kind: "LOCAL".into(),
            console_ip: "192.168.1.22".into(),
            console_port: 9304,
            mapped_ip: "192.168.1.10".into(),
            mapped_port: 40000,
        };
        let acc = accept_payload(2, 5123, 777, 0, &p, 1234, AcceptStyle::Original);
        assert!(
            acc.contains("\"localPeerAddr\":,\"localHashedId\":\"\"}}"),
            "the original's literal, byte for byte: {acc}"
        );
        assert!(acc.contains("\"candidate\":[{\"type\":\"LOCAL\",\"addr\":\"192.168.1.22\",\"mappedAddr\":\"192.168.1.10\",\"port\":9304,\"mappedPort\":40000}]"));
        let valid = accept_payload(2, 5123, 777, 0, &p, 1234, AcceptStyle::Valid);
        assert!(serde_json::from_str::<serde_json::Value>(valid.strip_prefix(PAYLOAD_PREFIX).unwrap()).is_ok());
        assert_eq!(
            valid.replace("{\"accountId\":\"1234\",\"platform\":\"REMOTE_PLAY\"}", ""),
            acc,
            "the two styles differ only in that value"
        );
    }

    #[test]
    fn the_session_message_wraps_the_payload_with_correct_escaping() {
        let payload = result_payload(3);
        let m = session_message_body(&payload, 18446744073709551615, "duid-1");
        let v: serde_json::Value = serde_json::from_str(&m).unwrap();
        assert_eq!(v["channel"], "remote_play:1");
        assert_eq!(v["payload"].as_str().unwrap(), payload);
        assert_eq!(v["to"][0]["accountId"], "18446744073709551615");
        assert_eq!(
            (v["to"][0]["deviceUniqueId"].as_str(), v["to"][0]["platform"].as_str()),
            (Some("duid-1"), Some("PS5"))
        );
    }

    #[test]
    fn incoming_messages_are_filtered_like_the_originals() {
        let msg = |sid: &str, ctx: &str| {
            WsMessage::parse(&format!(r#"{{"dataType":"{N_CUSTOM_DATA}","body":{{"data":{{"sessionId":"{sid}","customData1":"x"}}}},"contexts":[{ctx}]}}"#)).unwrap()
        };
        assert!(msg("s1", "").is_for_us("", "me"), "no contexts: accepted; session not known yet: accepted");
        assert!(msg("s1", r#"{"contextId":"me"}"#).is_for_us("s1", "me"));
        assert!(!msg("s1", r#"{"contextId":"other"}"#).is_for_us("s1", "me"), "someone else's context");
        assert!(!msg("s2", "").is_for_us("s1", "me"), "another session");
        assert!(!msg("", "").is_for_us("", "me"), "no session id");
        assert!(!WsMessage::parse(r#"{"dataType":"x","body":{}}"#).unwrap().is_for_us("", "me"));
        assert!(WsMessage::parse("not json").is_none());
        // a push message with a missing value or a trailing NUL is still read
        assert_eq!(
            WsMessage::parse("{\"dataType\":\"x\",\"body\":{\"data\":{\"sessionId\":\"s\",\"customData1\":}}}\0")
                .unwrap()
                .session_id(),
            "s"
        );
        assert_eq!(msg("s1", "").custom_data1(), "x");
    }

    #[test]
    fn console_payloads_parse_with_or_without_the_prefix_and_hostile_ones_do_not_panic() {
        let raw = r#"ver=1.0, type=text, body={"action":"OFFER","reqId":12,"error":0,"connRequest":{"sid":9001,"peerSid":0,"skey":"AAAAAAAAAAAAAAAAAAAAAA==","natType":2,"candidate":[{"type":"LOCAL","addr":"192.168.1.22","mappedAddr":"0.0.0.0","port":9304,"mappedPort":0},{"type":"STATIC","addr":"198.51.100.7","mappedAddr":"0.0.0.0","port":9304,"mappedPort":0}],"localHashedId":"cGVlcg=="}}"#;
        let p = EndpointPayload::parse(raw).unwrap();
        assert_eq!((p.action(), p.req_id, p.error), ("OFFER", Some(12), Some(0)));
        let c = p.conn_request.unwrap();
        assert_eq!((c.sid, c.nat_type, c.local_hashed_id.as_deref()), (Some(9001), Some(2), Some("cGVlcg==")));
        assert_eq!(c.candidate.len(), 2);
        assert_eq!(
            (c.candidate[0].kind.as_str(), c.candidate[0].addr.as_str(), c.candidate[0].port),
            ("LOCAL", "192.168.1.22", 9304)
        );
        assert!(EndpointPayload::parse(&raw.replace(PAYLOAD_PREFIX, "")).is_some());
        for bad in ["", "ver=1.0, type=text, body=", "{", "[]", "null", r#"{"action":5}"#, &"x".repeat(100_000)] {
            let _ = EndpointPayload::parse(bad); // never panics
        }
        assert_eq!(EndpointPayload::parse(r#"{"action":"RESULT","reqId":2,"error":0}"#).unwrap().action(), "RESULT");
    }

    #[test]
    fn the_websocket_host_comes_from_the_network_so_only_plain_host_names_pass() {
        assert_eq!(
            websocket_url("push.example.playstation.net").as_deref(),
            Some("wss://push.example.playstation.net/np/pushNotification")
        );
        for bad in
            ["", "evil.test/path", "a b", "host:443", "a..b", "x@y", "-", &"a".repeat(300), "evil.test?x=1", "ho\nst"]
        {
            assert!(websocket_url(bad).is_none(), "{bad:.20}");
        }
    }

    #[test]
    fn payloads_are_read_as_leniently_as_gson_does() {
        let ok = |t: &str| EndpointPayload::parse(t).map(|p| p.action().to_string());
        // the original's own ACCEPT (a missing value), with and without the prefix, and with junk around or after it
        let acc = "ver=1.0, type=text, body={\"action\":\"ACCEPT\",\"reqId\":2,\"error\":0,\"connRequest\":{\"sid\":1,\"candidate\":[],\"localPeerAddr\":,\"localHashedId\":\"\"}}";
        assert_eq!(ok(acc).as_deref(), Some("ACCEPT"));
        assert_eq!(ok(&format!("{acc}\0")).as_deref(), Some("ACCEPT"), "a trailing NUL");
        assert_eq!(ok(&format!("{acc}\r\n")).as_deref(), Some("ACCEPT"));
        assert_eq!(
            ok(&format!("  {acc} trailing words")).as_deref(),
            Some("ACCEPT"),
            "anything after the first value is ignored"
        );
        assert_eq!(ok("{\"action\":\"RESULT\",\"x\":}").as_deref(), Some("RESULT"), "a missing last value");
        assert_eq!(ok("{\"action\":\"OFFER\",\"list\":[1,,2]}"), None, "other breakage is still refused");
        // a colon inside a string is left alone
        assert_eq!(ok("{\"action\":\"a:,b\"}").as_deref(), Some("a:,b"));
        for bad in ["", "nothing", "{", "[1]"] {
            assert!(EndpointPayload::parse(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn redaction_keeps_the_structure_and_drops_addresses_ids_and_long_tokens() {
        let r = redact(
            r#"ver=1.0, type=text, body={"action":"OFFER","reqId":12,"connRequest":{"sid":9001,"candidate":[{"type":"LOCAL","addr":"192.168.1.22","port":9304}],"localPeerAddr":{"accountId":"4382906891241957369"},"localHashedId":"Y29uc29sZWlkZW50aWZpZXIxMjM0NQ=="}}"#,
        );
        assert!(
            r.contains("\"action\":\"OFFER\"")
                && r.contains("\"reqId\":12")
                && r.contains("\"sid\":9001")
                && r.contains("\"port\":9304"),
            "{r}"
        );
        for gone in ["192.168.1.22", "4382906891241957369", "Y29uc29sZWlk"] {
            assert!(!r.contains(gone), "{gone} survived: {r}");
        }
        assert!(redact(&"a".repeat(5000)).len() <= 800 * 3);
    }
}
