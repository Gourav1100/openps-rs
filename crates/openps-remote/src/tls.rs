//! Certificate verification for Sony's push server (feature `live`).
//!
//! Sony's push server sends an INCOMPLETE chain: the intermediate certificate is missing. Browsers and Java quietly download it from the
//! address inside the leaf certificate ("authority information access"); the original app switches that on for exactly three hosts
//! (`com.sun.security.allowedAIALocations`). rustls does not do it, so the connection failed with `UnknownIssuer`. This verifier does the
//! same, with the same restriction: when the normal verification says "unknown issuer", it fetches the missing intermediate over plain
//! HTTP from one of those three hosts and verifies again against the normal root set. A fetched certificate is never trusted by itself:
//! it only helps build a path to a root we already trust, so a wrong or malicious download can at worst make verification fail.

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::client::WebPkiServerVerifier;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{CertificateError, DigitallySignedStruct, Error, RootCertStore, SignatureScheme};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The only hosts an intermediate may be fetched from (the original's `allowedAIALocations`).
pub const ALLOWED_AIA_HOSTS: [&str; 3] = ["crt.comodoca.com", "crt.sectigo.com", "crt.usertrust.com"];
const MAX_DEPTH: usize = 3;
const MAX_CACHE: usize = 8;
const MAX_CERT_BYTES: usize = 64 * 1024;

/// DER encoding of the OID `id-ad-caIssuers` (1.3.6.1.5.5.7.48.2); it is followed by a `uniformResourceIdentifier` (context tag 6).
const CA_ISSUERS: [u8; 8] = [0x2B, 0x06, 0x01, 0x05, 0x05, 0x07, 0x30, 0x02];

/// The `caIssuers` URL in a certificate's authority-information-access extension. Reads bytes only; never panics on bad input.
pub fn ca_issuers_url(der: &[u8]) -> Option<String> {
    let at = der.windows(CA_ISSUERS.len()).position(|w| w == CA_ISSUERS)? + CA_ISSUERS.len();
    let rest = der.get(at..)?;
    if *rest.first()? != 0x86 {
        return None;
    }
    let (len, start) = match *rest.get(1)? {
        l if l < 0x80 => (usize::from(l), 2),
        0x81 => (usize::from(*rest.get(2)?), 3),
        _ => return None,
    };
    let url = rest.get(start..start + len)?;
    (url.is_ascii() && !url.iter().any(|b| b.is_ascii_control())).then(|| String::from_utf8_lossy(url).into_owned())
}

/// `http://<allowed host>/<path>`: nothing else is fetched (no other scheme, host, port, userinfo or odd characters).
pub fn aia_url_allowed(url: &str) -> Option<(&str, &str)> {
    let rest = url.strip_prefix("http://")?;
    let (host, path) = rest.split_once('/')?;
    let path_ok = path.len() <= 256
        && path.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b'/' | b'%' | b'~'));
    let path_ok =
        path_ok && !path.contains('%') && !path.split('/').any(|seg| seg.is_empty() || seg == "." || seg == "..");
    (ALLOWED_AIA_HOSTS.contains(&host) && path_ok).then_some((host, path))
}

/// Total time one certificate download may take (DNS, connect, request and answer together).
const FETCH_DEADLINE: Duration = Duration::from_secs(10);

/// Download a DER certificate over HTTP from an allowed host. The whole download (including the blocking name lookup) runs on a helper
/// thread and is abandoned after [`FETCH_DEADLINE`], so a stalling or trickling server cannot hold the TLS handshake.
pub fn http_fetch(url: &str) -> Option<Vec<u8>> {
    aia_url_allowed(url)?;
    let url = url.to_string();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(fetch_blocking(&url));
    });
    rx.recv_timeout(FETCH_DEADLINE).ok().flatten()
}

fn fetch_blocking(url: &str) -> Option<Vec<u8>> {
    let (host, path) = aia_url_allowed(url)?;
    let addr = (host, 80).to_socket_addrs().ok()?.next()?;
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(5)).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    s.set_write_timeout(Some(Duration::from_secs(5))).ok()?;
    s.write_all(
        format!("GET /{path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\nUser-Agent: OpenPS\r\n\r\n").as_bytes(),
    )
    .ok()?;
    let mut buf = Vec::new();
    s.take(MAX_CERT_BYTES as u64 + 4096).read_to_end(&mut buf).ok()?;
    parse_cert_response(&buf)
}

/// A `200` answer whose body looks like a DER certificate (an ASN.1 SEQUENCE, within the size limit).
pub fn parse_cert_response(buf: &[u8]) -> Option<Vec<u8>> {
    let split = buf.windows(4).position(|w| w == b"\r\n\r\n")?;
    let head = String::from_utf8_lossy(&buf[..split]);
    if head.split_whitespace().nth(1) != Some("200") {
        return None;
    }
    let body = &buf[split + 4..];
    (body.len() >= 16 && body.len() <= MAX_CERT_BYTES && body[0] == 0x30).then(|| body.to_vec())
}

pub type Fetcher = Arc<dyn Fn(&str) -> Option<Vec<u8>> + Send + Sync>;

/// The normal WebPKI verifier plus the missing-intermediate step.
pub struct AiaVerifier {
    inner: Arc<WebPkiServerVerifier>,
    fetch: Fetcher,
    cache: Mutex<HashMap<String, Vec<u8>>>,
}

impl std::fmt::Debug for AiaVerifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AiaVerifier")
    }
}

impl AiaVerifier {
    pub fn new(
        roots: Arc<RootCertStore>,
        provider: Arc<rustls::crypto::CryptoProvider>,
        fetch: Fetcher,
    ) -> Result<Self, rustls::client::VerifierBuilderError> {
        let inner = WebPkiServerVerifier::builder_with_provider(roots, provider).build()?;
        Ok(Self { inner, fetch, cache: Mutex::new(HashMap::new()) })
    }

    fn intermediate(&self, url: &str) -> Option<Vec<u8>> {
        aia_url_allowed(url)?; // checked here as well: the fetcher is replaceable
        if let Some(c) = self.cache.lock().ok()?.get(url) {
            return Some(c.clone());
        }
        let der = (self.fetch)(url)?;
        let mut cache = self.cache.lock().ok()?;
        if cache.len() >= MAX_CACHE {
            cache.clear();
        }
        cache.insert(url.to_string(), der.clone());
        Some(der)
    }
}

impl ServerCertVerifier for AiaVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        let first = self.inner.verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now);
        if !matches!(first, Err(Error::InvalidCertificate(CertificateError::UnknownIssuer))) {
            return first;
        }
        let mut chain: Vec<CertificateDer<'static>> = intermediates.iter().map(|c| c.clone().into_owned()).collect();
        let mut tip: Vec<u8> = chain.last().map_or_else(|| end_entity.as_ref().to_vec(), |c| c.as_ref().to_vec());
        for _ in 0..MAX_DEPTH {
            let Some(der) = ca_issuers_url(&tip).and_then(|u| self.intermediate(&u)) else { break };
            chain.push(CertificateDer::from(der.clone()));
            match self.inner.verify_server_cert(end_entity, &chain, server_name, ocsp_response, now) {
                Err(Error::InvalidCertificate(CertificateError::UnknownIssuer)) => tip = der, // one more level up
                other => return other,
            }
        }
        first
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        self.inner.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        self.inner.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rcgen::{BasicConstraints, CertificateParams, CustomExtension, DnType, IsCa, KeyPair, KeyUsagePurpose};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn provider() -> Arc<rustls::crypto::CryptoProvider> {
        Arc::new(rustls::crypto::ring::default_provider())
    }

    /// The AIA extension (OID 1.3.6.1.5.5.7.1.1) with one caIssuers entry.
    fn aia_ext(url: &str) -> CustomExtension {
        let u = url.as_bytes();
        let mut desc = vec![0x06, 0x08];
        desc.extend(CA_ISSUERS);
        desc.push(0x86);
        desc.push(u.len() as u8);
        desc.extend(u);
        let mut seq = vec![0x30, desc.len() as u8];
        seq.extend(desc);
        let mut outer = vec![0x30, seq.len() as u8];
        outer.extend(seq);
        CustomExtension::from_oid_content(&[1, 3, 6, 1, 5, 5, 7, 1, 1], outer)
    }

    struct Pki {
        root: rcgen::Certificate,
        intermediate_der: Vec<u8>,
        leaf_der: Vec<u8>,
        leaf_without_aia_der: Vec<u8>,
    }

    fn pki(aia: &str) -> Pki {
        let root_key = KeyPair::generate().unwrap();
        let mut rp = CertificateParams::new(vec![]).unwrap();
        rp.distinguished_name.push(DnType::CommonName, "Test Root");
        rp.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        rp.key_usages = vec![KeyUsagePurpose::KeyCertSign];
        let root = rp.self_signed(&root_key).unwrap();
        let root_issuer = rcgen::Issuer::new(rp, root_key);

        let int_key = KeyPair::generate().unwrap();
        let mut ip = CertificateParams::new(vec![]).unwrap();
        ip.distinguished_name.push(DnType::CommonName, "Test Intermediate");
        ip.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ip.key_usages = vec![KeyUsagePurpose::KeyCertSign];
        let inter = ip.signed_by(&int_key, &root_issuer).unwrap();
        let int_issuer = rcgen::Issuer::new(ip, int_key);

        let leaf_key = KeyPair::generate().unwrap();
        let mut lp = CertificateParams::new(vec!["push.example.net".to_string()]).unwrap();
        lp.custom_extensions.push(aia_ext(aia));
        let leaf = lp.signed_by(&leaf_key, &int_issuer).unwrap();
        let mut lp2 = CertificateParams::new(vec!["push.example.net".to_string()]).unwrap();
        lp2.distinguished_name.push(DnType::CommonName, "no-aia");
        let leaf2 = lp2.signed_by(&leaf_key, &int_issuer).unwrap();
        Pki {
            intermediate_der: inter.der().to_vec(),
            leaf_der: leaf.der().to_vec(),
            leaf_without_aia_der: leaf2.der().to_vec(),
            root,
        }
    }

    fn verifier(p: &Pki, fetch: Fetcher) -> AiaVerifier {
        let mut roots = RootCertStore::empty();
        roots.add(p.root.der().clone()).unwrap();
        AiaVerifier::new(Arc::new(roots), provider(), fetch).unwrap()
    }

    fn check(v: &AiaVerifier, leaf: &[u8], sent: &[Vec<u8>]) -> Result<ServerCertVerified, Error> {
        let inter: Vec<CertificateDer<'static>> = sent.iter().map(|d| CertificateDer::from(d.clone())).collect();
        v.verify_server_cert(
            &CertificateDer::from(leaf.to_vec()),
            &inter,
            &ServerName::try_from("push.example.net").unwrap(),
            &[],
            UnixTime::now(),
        )
    }

    #[test]
    fn the_caissuers_url_is_read_from_a_real_certificate_and_garbage_gives_nothing() {
        let p = pki("http://crt.sectigo.com/Test_CA.crt");
        assert_eq!(ca_issuers_url(&p.leaf_der).as_deref(), Some("http://crt.sectigo.com/Test_CA.crt"));
        assert_eq!(ca_issuers_url(&p.leaf_without_aia_der), None);
        for bad in [
            &b""[..],
            &[0x2B, 0x06, 0x01, 0x05, 0x05, 0x07, 0x30, 0x02][..],
            &[0x2B, 0x06, 0x01, 0x05, 0x05, 0x07, 0x30, 0x02, 0x86][..],
            &[0x2B, 0x06, 0x01, 0x05, 0x05, 0x07, 0x30, 0x02, 0x86, 0x50, b'a'][..],
            &[0xFF; 100][..],
        ] {
            assert_eq!(ca_issuers_url(bad), None);
        }
    }

    #[test]
    fn only_the_three_allowed_hosts_and_sane_paths_are_ever_fetched() {
        for ok in [
            "http://crt.sectigo.com/a.crt",
            "http://crt.comodoca.com/COMODO.crt",
            "http://crt.usertrust.com/x/y-1_2.crt",
        ] {
            assert!(aia_url_allowed(ok).is_some(), "{ok}");
        }
        for bad in [
            "https://crt.sectigo.com/a.crt",
            "http://evil.example/a.crt",
            "http://crt.sectigo.com.evil.example/a.crt",
            "http://crt.sectigo.com:8080/a.crt",
            "http://user@crt.sectigo.com/a.crt",
            "http://crt.sectigo.com/a b.crt",
            "http://crt.sectigo.com/a?x=1",
            "http://crt.sectigo.com",
            "ftp://crt.sectigo.com/a",
            &format!("http://crt.sectigo.com/{}", "a".repeat(300)),
        ] {
            assert!(aia_url_allowed(bad).is_none(), "{bad:.50}");
        }
    }

    #[test]
    fn a_missing_intermediate_is_fetched_and_the_chain_then_verifies() {
        let p = pki("http://crt.sectigo.com/Test_CA.crt");
        let fetched = Arc::new(AtomicUsize::new(0));
        let (f2, der) = (fetched.clone(), p.intermediate_der.clone());
        let v = verifier(
            &p,
            Arc::new(move |url| {
                assert_eq!(url, "http://crt.sectigo.com/Test_CA.crt");
                f2.fetch_add(1, Ordering::Relaxed);
                Some(der.clone())
            }),
        );
        // the server sends only the leaf, as Sony's does
        assert!(check(&v, &p.leaf_der, &[]).is_ok());
        assert_eq!(fetched.load(Ordering::Relaxed), 1);
        // a second connection uses the cache
        assert!(check(&v, &p.leaf_der, &[]).is_ok());
        assert_eq!(fetched.load(Ordering::Relaxed), 1, "cached");
    }

    #[test]
    fn a_complete_chain_never_triggers_a_download() {
        let p = pki("http://crt.sectigo.com/Test_CA.crt");
        let v = verifier(&p, Arc::new(|_| panic!("must not fetch when the chain is complete")));
        assert!(check(&v, &p.leaf_der, std::slice::from_ref(&p.intermediate_der)).is_ok());
    }

    #[test]
    fn nothing_is_trusted_just_because_it_was_downloaded() {
        let p = pki("http://crt.sectigo.com/Test_CA.crt");
        // an unrelated certificate comes back: verification still fails
        let other = pki("http://crt.sectigo.com/Other.crt");
        let wrong = other.intermediate_der.clone();
        let v = verifier(&p, Arc::new(move |_| Some(wrong.clone())));
        assert!(check(&v, &p.leaf_der, &[]).is_err());
        // nothing comes back
        let v = verifier(&p, Arc::new(|_| None));
        assert!(matches!(check(&v, &p.leaf_der, &[]), Err(Error::InvalidCertificate(CertificateError::UnknownIssuer))));
        // garbage comes back
        let v = verifier(&p, Arc::new(|_| Some(vec![0x30, 0x03, 1, 2, 3])));
        assert!(check(&v, &p.leaf_der, &[]).is_err());
        // no AIA at all in the leaf: nothing to fetch
        let v = verifier(&p, Arc::new(|_| panic!("no AIA, no fetch")));
        assert!(check(&v, &p.leaf_without_aia_der, &[]).is_err());
    }

    #[test]
    fn a_leaf_pointing_at_a_foreign_host_is_not_followed() {
        let p = pki("http://evil.example/Test_CA.crt");
        let der = p.intermediate_der.clone();
        let v = verifier(&p, Arc::new(move |_| Some(der.clone()))); // the fetcher would happily answer: the allowlist stops it first
        assert!(check(&v, &p.leaf_der, &[]).is_err());
    }

    #[test]
    fn a_wrong_host_name_is_still_refused_even_with_a_valid_chain() {
        let p = pki("http://crt.sectigo.com/Test_CA.crt");
        let der = p.intermediate_der.clone();
        let v = verifier(&p, Arc::new(move |_| Some(der.clone())));
        let inter = [CertificateDer::from(p.intermediate_der.clone())];
        let r = v.verify_server_cert(
            &CertificateDer::from(p.leaf_der.clone()),
            &inter,
            &ServerName::try_from("other.example.net").unwrap(),
            &[],
            UnixTime::now(),
        );
        assert!(r.is_err());
    }

    #[test]
    fn http_answers_are_parsed_strictly() {
        let der = {
            let mut d = vec![0x30, 0x82, 0x01, 0x00];
            d.extend(vec![0u8; 20]);
            d
        };
        let mk = |status: &str, body: &[u8]| {
            let mut r = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\n\r\n", body.len()).into_bytes();
            r.extend(body);
            r
        };
        assert_eq!(parse_cert_response(&mk("200 OK", &der)), Some(der.clone()));
        assert_eq!(parse_cert_response(&mk("404 Not Found", &der)), None);
        assert_eq!(parse_cert_response(&mk("200 OK", b"<html>not a certificate</html>")), None);
        assert_eq!(parse_cert_response(&mk("200 OK", &vec![0x30; MAX_CERT_BYTES + 1])), None);
        assert_eq!(parse_cert_response(b"garbage"), None);
        assert_eq!(parse_cert_response(&mk("200 OK", &[0x30, 1, 2])), None, "too short to be a certificate");
    }
}
