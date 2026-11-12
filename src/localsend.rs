use crate::store;
use anyhow::{Context, Result, anyhow, bail, ensure};
use bytes::Bytes;
use futures_util::StreamExt;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full, Limited, StreamBody};
use hyper::body::{Frame, Incoming};
use hyper::header::{CONTENT_LENGTH, CONTENT_TYPE, HOST};
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, WebPkiSupportedAlgorithms, verify_tls12_signature, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{DigitallySignedStruct, DistinguishedName, SignatureScheme};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream, UdpSocket};

pub const PORT: u16 = 53317;
const GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 167);
const VERSION: &str = "2.2";
const FRESH: Duration = Duration::from_secs(660);
const API: &str = "/api/localsend/v2";

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Off,
    Ask,
    Always,
}

impl Mode {
    pub fn platform_default() -> Self {
        if cfg!(target_os = "macos") { Mode::Ask } else { Mode::Off }
    }
}

pub trait Host: Send + Sync + 'static {
    fn alias(&self) -> String;
    fn inbox(&self) -> PathBuf;
    fn mode(&self) -> Mode;
    fn trusted(&self, fingerprint: &str) -> bool;
    fn trust(&self, fingerprint: &str);
    fn received(&self, from: &str, path: &Path);
    fn text(&self, from: &str, text: &str);
    fn image(&self, from: &str, png: Vec<u8>);
}

pub const CLIP_IMAGE: &str = ".drop-clipboard.png";

#[derive(Clone, Debug, Serialize)]
pub struct Peer {
    pub alias: String,
    pub fingerprint: String,
    pub ip: IpAddr,
    pub port: u16,
    pub https: bool,
    pub device_type: Option<String>,
    pub model: Option<String>,
    #[serde(skip)]
    pub seen: Instant,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
struct Info {
    alias: String,
    #[serde(default)]
    version: String,
    #[serde(default)]
    device_model: Option<String>,
    #[serde(default)]
    device_type: Option<String>,
    #[serde(default)]
    fingerprint: String,
    #[serde(default = "default_port")]
    port: u16,
    #[serde(default = "default_protocol")]
    protocol: String,
    #[serde(default)]
    download: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    announce: Option<bool>,
}

fn default_port() -> u16 {
    PORT
}

fn default_protocol() -> String {
    "https".into()
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
struct FileDto {
    id: String,
    file_name: String,
    size: u64,
    #[serde(default)]
    file_type: String,
    #[serde(default)]
    sha256: Option<String>,
    #[serde(default)]
    preview: Option<String>,
}

#[derive(Deserialize)]
struct PrepareReq {
    info: Info,
    files: HashMap<String, FileDto>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PrepareResp {
    session_id: String,
    files: HashMap<String, String>,
}

struct Upload {
    id: String,
    ip: IpAddr,
    from: String,
    files: HashMap<String, (FileDto, String)>,
    done: usize,
    touched: Instant,
}

pub struct LocalSend {
    host: Arc<dyn Host>,
    fingerprint: String,
    provider: Arc<CryptoProvider>,
    cert: CertificateDer<'static>,
    key: PrivatePkcs8KeyDer<'static>,
    peers: Mutex<HashMap<String, Peer>>,
    session: Mutex<Option<Upload>>,
    udp: Arc<UdpSocket>,
    send_lock: Mutex<()>,
    last_announce: Mutex<Option<Instant>>,
}

type Body = BoxBody<Bytes, std::io::Error>;

fn fingerprint(der: &[u8]) -> String {
    Sha256::digest(der).iter().map(|b| format!("{b:02X}")).collect()
}

fn identity() -> Result<(CertificateDer<'static>, PrivatePkcs8KeyDer<'static>)> {
    let dir = store::data_dir();
    let (cp, kp) = (dir.join("localsend.cert"), dir.join("localsend.key"));
    if let (Ok(c), Ok(k)) = (std::fs::read(&cp), std::fs::read(&kp)) {
        return Ok((CertificateDer::from(c), PrivatePkcs8KeyDer::from(k)));
    }
    let key = rcgen::KeyPair::generate()?;
    let mut params = rcgen::CertificateParams::new(Vec::<String>::new())?;
    params.distinguished_name = rcgen::DistinguishedName::new();
    params.distinguished_name.push(rcgen::DnType::CommonName, "drop");
    let cert = params.self_signed(&key)?;
    let (c, k) = (cert.der().to_vec(), key.serialize_der());
    std::fs::write(&cp, &c)?;
    std::fs::write(&kp, &k)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&kp, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok((CertificateDer::from(c), PrivatePkcs8KeyDer::from(k)))
}

#[derive(Debug)]
struct AnyClient(WebPkiSupportedAlgorithms);

impl ClientCertVerifier for AnyClient {
    fn offer_client_auth(&self) -> bool {
        true
    }

    fn client_auth_mandatory(&self) -> bool {
        false
    }

    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(&self, _: &CertificateDer<'_>, _: &[CertificateDer<'_>], _: UnixTime) -> Result<ClientCertVerified, rustls::Error> {
        Ok(ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(&self, m: &[u8], c: &CertificateDer<'_>, d: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(m, c, d, &self.0)
    }

    fn verify_tls13_signature(&self, m: &[u8], c: &CertificateDer<'_>, d: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(m, c, d, &self.0)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.supported_schemes()
    }
}

#[derive(Debug)]
struct Pinned {
    expect: String,
    algs: WebPkiSupportedAlgorithms,
}

impl ServerCertVerifier for Pinned {
    fn verify_server_cert(
        &self,
        end: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if !self.expect.is_empty() && !fingerprint(end).eq_ignore_ascii_case(&self.expect) {
            return Err(rustls::Error::General("LocalSend certificate does not match the device".into()));
        }
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(&self, m: &[u8], c: &CertificateDer<'_>, d: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(m, c, d, &self.algs)
    }

    fn verify_tls13_signature(&self, m: &[u8], c: &CertificateDer<'_>, d: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(m, c, d, &self.algs)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algs.supported_schemes()
    }
}

fn bind_udp() -> Result<UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};
    let s = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    s.set_reuse_address(true)?;
    #[cfg(unix)]
    s.set_reuse_port(true)?;
    s.bind(&SocketAddr::from((Ipv4Addr::UNSPECIFIED, PORT)).into())?;
    s.set_multicast_loop_v4(true)?;
    s.set_multicast_ttl_v4(1)?;
    s.set_nonblocking(true)?;
    Ok(UdpSocket::from_std(s.into())?)
}

#[cfg(unix)]
fn interfaces() -> Vec<Ipv4Addr> {
    let mut out = Vec::new();
    unsafe {
        let mut ifap: *mut libc::ifaddrs = std::ptr::null_mut();
        if libc::getifaddrs(&mut ifap) != 0 {
            return out;
        }
        let mut cur = ifap;
        while !cur.is_null() {
            let ifa = &*cur;
            let flags = ifa.ifa_flags as libc::c_int;
            let wanted = libc::IFF_UP | libc::IFF_MULTICAST;
            if !ifa.ifa_addr.is_null()
                && (*ifa.ifa_addr).sa_family as libc::c_int == libc::AF_INET
                && flags & wanted == wanted
                && flags & (libc::IFF_LOOPBACK | libc::IFF_POINTOPOINT) == 0
            {
                let sin = &*(ifa.ifa_addr as *const libc::sockaddr_in);
                out.push(Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr)));
            }
            cur = ifa.ifa_next;
        }
        libc::freeifaddrs(ifap);
    }
    out.retain(|ip| !(ip.octets()[0] == 100 && ip.octets()[1] & 0xc0 == 64) && !ip.is_link_local());
    out.sort();
    out.dedup();
    if out.is_empty() {
        out.push(Ipv4Addr::UNSPECIFIED);
    }
    out
}

#[cfg(not(unix))]
fn interfaces() -> Vec<Ipv4Addr> {
    vec![Ipv4Addr::UNSPECIFIED]
}

fn reply(status: StatusCode, body: Option<serde_json::Value>) -> Response<Full<Bytes>> {
    let mut r = Response::new(Full::new(body.map(|b| Bytes::from(b.to_string())).unwrap_or_default()));
    *r.status_mut() = status;
    r.headers_mut().insert(CONTENT_TYPE, "application/json".parse().unwrap());
    r
}

fn query(q: &str) -> HashMap<String, String> {
    q.split('&')
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| (decode(k), decode(v)))
        .collect()
}

fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                match u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or(""), 16) {
                    Ok(v) => {
                        out.push(v);
                        i += 3;
                        continue;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            b'+' => out.push(b' '),
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn encode(s: &str) -> String {
    s.bytes()
        .map(|c| match c {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (c as char).to_string(),
            c => format!("%{c:02X}"),
        })
        .collect()
}

fn token() -> String {
    data_encoding::HEXLOWER.encode(&rand::random::<[u8; 16]>())
}

fn mime(path: &Path) -> &'static str {
    let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    match ext.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "heic" => "image/heic",
        "webp" => "image/webp",
        "mp4" => "video/mp4",
        "mov" => "video/quicktime",
        "mp3" => "audio/mpeg",
        "m4a" => "audio/mp4",
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        "apk" => "application/vnd.android.package-archive",
        "txt" | "md" => "text/plain",
        "json" => "application/json",
        _ => "application/octet-stream",
    }
}

pub(crate) fn human(n: u64) -> String {
    let mut v = n as f64;
    let mut u = 0;
    let units = ["B", "KB", "MB", "GB", "TB"];
    while v >= 1000.0 && u < units.len() - 1 {
        v /= 1000.0;
        u += 1;
    }
    if u == 0 { format!("{n} B") } else { format!("{v:.1} {}", units[u]) }
}

pub async fn ask(title: &str, text: &str, buttons: &[&str]) -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
        let list = buttons.iter().map(|b| format!("\"{}\"", esc(b))).collect::<Vec<_>>().join(", ");
        let script = format!(
            "display dialog \"{}\" with title \"{}\" buttons {{{}}} default button {} giving up after 120",
            esc(text),
            esc(title),
            list,
            buttons.len()
        );
        let out = tokio::process::Command::new("osascript").args(["-e", &script]).output().await.ok()?;
        let s = String::from_utf8_lossy(&out.stdout);
        if s.contains("gave up:true") {
            return None;
        }
        s.split("button returned:").nth(1).map(|r| r.split(", gave up").next().unwrap_or("").trim().to_string())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (title, text, buttons);
        None
    }
}

impl LocalSend {
    pub async fn start(host: Arc<dyn Host>) -> Result<Arc<Self>> {
        let (cert, key) = identity()?;
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let server = rustls::ServerConfig::builder_with_provider(provider.clone())
            .with_safe_default_protocol_versions()?
            .with_client_cert_verifier(Arc::new(AnyClient(provider.signature_verification_algorithms)))
            .with_single_cert(vec![cert.clone()], PrivateKeyDer::Pkcs8(key.clone_key()))?;
        let mut server = server;
        server.alpn_protocols = vec![b"http/1.1".to_vec()];
        let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, PORT))
            .await
            .with_context(|| format!("port {PORT} is taken (is the LocalSend app running?)"))?;
        let udp = Arc::new(bind_udp()?);
        let ls = Arc::new(LocalSend {
            host,
            fingerprint: fingerprint(&cert),
            provider,
            cert,
            key,
            peers: Mutex::new(HashMap::new()),
            session: Mutex::new(None),
            udp,
            send_lock: Mutex::new(()),
            last_announce: Mutex::new(None),
        });
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server));
        {
            let ls = ls.clone();
            tokio::spawn(async move {
                loop {
                    let Ok((tcp, addr)) = listener.accept().await else { continue };
                    let (ls, acceptor) = (ls.clone(), acceptor.clone());
                    tokio::spawn(async move {
                        let _ = tcp.set_nodelay(true);
                        let Ok(Ok(tls)) = tokio::time::timeout(Duration::from_secs(10), acceptor.accept(tcp)).await else { return };
                        let cert_fp = tls.get_ref().1.peer_certificates().and_then(|c| c.first()).map(|c| fingerprint(c));
                        let ip = canonical(addr.ip());
                        let svc = hyper::service::service_fn(move |req| {
                            let (ls, cert_fp) = (ls.clone(), cert_fp.clone());
                            async move { Ok::<_, std::convert::Infallible>(ls.route(ip, cert_fp, req).await) }
                        });
                        let _ = hyper::server::conn::http1::Builder::new().serve_connection(TokioIo::new(tls), svc).await;
                    });
                }
            });
        }
        tokio::spawn(ls.clone().listen());
        {
            let ls = ls.clone();
            tokio::spawn(async move {
                loop {
                    ls.announce().await;
                    tokio::time::sleep(Duration::from_secs(300)).await;
                }
            });
        }
        Ok(ls)
    }

    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    fn info(&self, announce: Option<bool>) -> Info {
        Info {
            alias: self.host.alias(),
            version: VERSION.into(),
            device_model: Some(if cfg!(target_os = "macos") { "macOS" } else { "Linux" }.into()),
            device_type: Some("desktop".into()),
            fingerprint: self.fingerprint.clone(),
            port: PORT,
            protocol: "https".into(),
            download: false,
            announce,
        }
    }

    fn on(&self) -> bool {
        self.host.mode() != Mode::Off
    }

    fn multicast(&self, info: &Info) {
        let Ok(msg) = serde_json::to_vec(info) else { return };
        let _g = self.send_lock.lock().unwrap();
        let sock = socket2::SockRef::from(&*self.udp);
        for iface in interfaces() {
            let _ = self.udp.join_multicast_v4(GROUP, iface);
            if sock.set_multicast_if_v4(&iface).is_ok() {
                let _ = self.udp.try_send_to(&msg, SocketAddr::from((GROUP, PORT)));
            }
        }
    }

    pub async fn announce(&self) {
        if !self.on() {
            return;
        }
        *self.last_announce.lock().unwrap() = Some(Instant::now());
        for d in [100, 500, 2000] {
            tokio::time::sleep(Duration::from_millis(d)).await;
            self.multicast(&self.info(Some(true)));
        }
    }

    pub fn nudge(self: &Arc<Self>) {
        let due = self.last_announce.lock().unwrap().is_none_or(|t| t.elapsed() > Duration::from_secs(30));
        if due {
            let ls = self.clone();
            tokio::spawn(async move { ls.announce().await });
        }
    }

    async fn listen(self: Arc<Self>) {
        let mut buf = vec![0u8; 65536];
        loop {
            let Ok((n, src)) = self.udp.recv_from(&mut buf).await else {
                tokio::time::sleep(Duration::from_millis(200)).await;
                continue;
            };
            let Ok(info) = serde_json::from_slice::<Info>(&buf[..n]) else { continue };
            if info.fingerprint == self.fingerprint || !self.on() {
                continue;
            }
            let peer = self.remember(&info, canonical(src.ip()), info.fingerprint.clone());
            if info.announce == Some(true) {
                let ls = self.clone();
                tokio::spawn(async move {
                    let body = serde_json::to_vec(&ls.info(None)).unwrap_or_default();
                    let ok = ls
                        .http(&peer, Method::POST, &format!("{API}/register"), json_body(body), None, Duration::from_secs(5))
                        .await
                        .is_ok_and(|(s, _)| s.is_success());
                    if !ok {
                        ls.multicast(&ls.info(Some(false)));
                    }
                });
            }
        }
    }

    fn remember(&self, info: &Info, ip: IpAddr, fp: String) -> Peer {
        if fp.eq_ignore_ascii_case(&self.fingerprint) {
            return Peer { alias: info.alias.clone(), fingerprint: fp, ip, port: info.port, https: true, device_type: None, model: None, seen: Instant::now() };
        }
        let peer = Peer {
            alias: info.alias.clone(),
            fingerprint: fp.clone(),
            ip,
            port: info.port,
            https: info.protocol != "http",
            device_type: info.device_type.clone(),
            model: info.device_model.clone(),
            seen: Instant::now(),
        };
        self.peers.lock().unwrap().insert(fp, peer.clone());
        peer
    }

    pub fn peers(&self) -> Vec<Peer> {
        let mut v: Vec<Peer> = self.peers.lock().unwrap().values().filter(|p| p.seen.elapsed() < FRESH).cloned().collect();
        v.sort_by(|a, b| a.alias.cmp(&b.alias));
        v
    }


    pub fn find(&self, q: &str) -> Option<Peer> {
        let q = q.to_lowercase();
        let peers = self.peers();
        peers
            .iter()
            .find(|p| p.alias.to_lowercase() == q || p.fingerprint.to_lowercase() == q)
            .or_else(|| peers.iter().find(|p| p.alias.to_lowercase().starts_with(&q)))
            .cloned()
    }

    async fn route(self: &Arc<Self>, ip: IpAddr, cert_fp: Option<String>, req: Request<Incoming>) -> Response<Full<Bytes>> {
        if !self.on() {
            return reply(StatusCode::FORBIDDEN, None);
        }
        let path = req.uri().path().to_string();
        let q = query(req.uri().query().unwrap_or(""));
        let r = match (req.method().clone(), path.strip_prefix(API).unwrap_or("")) {
            (Method::POST, "/register") => self.on_register(ip, cert_fp, req).await,
            (Method::GET, "/info") => Ok(reply(StatusCode::OK, Some(json!(self.info(None))))),
            (Method::POST, "/prepare-upload") => self.on_prepare(ip, cert_fp, req).await,
            (Method::POST, "/upload") => self.on_upload(ip, q, req).await,
            (Method::POST, "/cancel") => {
                let mut s = self.session.lock().unwrap();
                if s.as_ref().is_some_and(|u| u.ip == ip && q.get("sessionId") == Some(&u.id)) {
                    *s = None;
                }
                Ok(reply(StatusCode::OK, None))
            }
            _ => Ok(reply(StatusCode::NOT_FOUND, None)),
        };
        r.unwrap_or_else(|e| {
            eprintln!("localsend {path} from {ip}: {e:#}");
            reply(StatusCode::BAD_REQUEST, Some(json!({"message": format!("{e:#}")})))
        })
    }

    async fn on_register(&self, ip: IpAddr, cert_fp: Option<String>, req: Request<Incoming>) -> Result<Response<Full<Bytes>>> {
        let info: Info = read_json(req.into_body(), 1 << 16).await?;
        let fp = cert_fp.unwrap_or_else(|| info.fingerprint.clone());
        self.remember(&info, ip, fp);
        Ok(reply(StatusCode::OK, Some(json!(self.info(None)))))
    }

    async fn on_prepare(&self, ip: IpAddr, cert_fp: Option<String>, req: Request<Incoming>) -> Result<Response<Full<Bytes>>> {
        let p: PrepareReq = read_json(req.into_body(), 16 << 20).await?;
        let fp = cert_fp.unwrap_or_else(|| p.info.fingerprint.clone());
        let from = p.info.alias.clone();
        self.remember(&p.info, ip, fp.clone());
        if let Some(u) = self.session.lock().unwrap().as_ref() {
            if u.touched.elapsed() < Duration::from_secs(600) {
                return Ok(reply(StatusCode::CONFLICT, Some(json!({"message": "Blocked by another session"}))));
            }
        }
        let trusted = self.host.trusted(&fp) || self.host.mode() == Mode::Always;
        if p.files.len() == 1 {
            let f = p.files.values().next().unwrap();
            if f.file_type.starts_with("text/") && f.preview.is_some() {
                let text = f.preview.clone().unwrap_or_default();
                let take = trusted || {
                    let shown: String = text.chars().take(300).collect();
                    ask("drop", &format!("{from} sent you text:\n\n{shown}"), &["Ignore", "Copy"]).await.as_deref() == Some("Copy")
                };
                if take {
                    self.host.text(&from, &text);
                }
                return Ok(reply(StatusCode::NO_CONTENT, None));
            }
        }
        let clip_image = p.files.len() == 1
            && p.files.values().next().is_some_and(|f| f.file_name == CLIP_IMAGE && f.file_type.starts_with("image/"));
        if clip_image {
            if p.files.values().next().is_some_and(|f| f.size > crate::clip::MAX_IMAGE) {
                return Ok(reply(StatusCode::PAYLOAD_TOO_LARGE, Some(json!({"message": "Image too large"}))));
            }
            if !trusted && ask("drop", &format!("{from} sent you an image for the clipboard"), &["Ignore", "Copy"]).await.as_deref() != Some("Copy") {
                return Ok(reply(StatusCode::FORBIDDEN, Some(json!({"message": "Rejected"}))));
            }
        } else if !trusted {
            let total: u64 = p.files.values().map(|f| f.size).sum();
            let mut names: Vec<&str> = p.files.values().map(|f| f.file_name.as_str()).collect();
            names.sort();
            let shown = if names.len() > 4 { format!("{}, and {} more", names[..4].join(", "), names.len() - 4) } else { names.join(", ") };
            let what = if p.files.len() == 1 { "a file".to_string() } else { format!("{} files", p.files.len()) };
            let msg = format!("{from} wants to send you {what} ({}):\n\n{shown}", human(total));
            match ask("drop", &msg, &["Decline", "Always accept", "Accept"]).await.as_deref() {
                Some("Accept") => {}
                Some("Always accept") => self.host.trust(&fp),
                _ => return Ok(reply(StatusCode::FORBIDDEN, Some(json!({"message": "Rejected"})))),
            }
        }
        let id = token();
        let files: HashMap<String, (FileDto, String)> = p.files.into_iter().map(|(k, f)| (k, (f, token()))).collect();
        let tokens: HashMap<String, String> = files.iter().map(|(k, (_, t))| (k.clone(), t.clone())).collect();
        *self.session.lock().unwrap() = Some(Upload { id: id.clone(), ip, from, files, done: 0, touched: Instant::now() });
        Ok(reply(StatusCode::OK, Some(json!(PrepareResp { session_id: id, files: tokens }))))
    }

    async fn on_upload(&self, ip: IpAddr, q: HashMap<String, String>, req: Request<Incoming>) -> Result<Response<Full<Bytes>>> {
        let (Some(sid), Some(fid), Some(tok)) = (q.get("sessionId"), q.get("fileId"), q.get("token")) else {
            return Ok(reply(StatusCode::BAD_REQUEST, Some(json!({"message": "Missing parameters"}))));
        };
        let (file, from) = {
            let mut s = self.session.lock().unwrap();
            match s.as_mut() {
                Some(u) if &u.id == sid && u.ip == ip => match u.files.get(fid) {
                    Some((f, t)) if t == tok => {
                        u.touched = Instant::now();
                        (f.clone(), u.from.clone())
                    }
                    _ => return Ok(reply(StatusCode::FORBIDDEN, Some(json!({"message": "Invalid token"})))),
                },
                _ => return Ok(reply(StatusCode::FORBIDDEN, Some(json!({"message": "Invalid session"})))),
            }
        };
        let inbox = self.host.inbox();
        tokio::fs::create_dir_all(&inbox).await?;
        let (dest, part, f) = {
            let (inbox, name) = (inbox.clone(), file.file_name.clone());
            tokio::task::spawn_blocking(move || store::reserve(&inbox, &name)).await??
        };
        let mut out = tokio::io::BufWriter::with_capacity(1 << 20, tokio::fs::File::from_std(f));
        let mut hasher = file.sha256.as_ref().map(|_| Sha256::new());
        let mut body = req.into_body();
        let mut got = 0u64;
        let result: Result<()> = async {
            while let Some(frame) = body.frame().await {
                if let Ok(data) = frame?.into_data() {
                    got += data.len() as u64;
                    ensure!(got <= file.size, "{} is longer than announced", file.file_name);
                    if let Some(h) = hasher.as_mut() {
                        h.update(&data);
                    }
                    out.write_all(&data).await?;
                }
            }
            out.flush().await?;
            Ok(())
        }
        .await;
        if let Err(e) = result {
            let _ = tokio::fs::remove_file(&part).await;
            return Err(e);
        }
        if let (Some(h), Some(want)) = (hasher, file.sha256.as_ref()) {
            let got = data_encoding::HEXLOWER.encode(&h.finalize());
            if !got.eq_ignore_ascii_case(want) {
                let _ = tokio::fs::remove_file(&part).await;
                return Ok(reply(StatusCode::UNPROCESSABLE_ENTITY, Some(json!({"message": "Checksum mismatch"}))));
            }
        }
        if file.file_name == CLIP_IMAGE {
            let png = tokio::fs::read(&part).await;
            let _ = tokio::fs::remove_file(&part).await;
            self.host.image(&from, png?);
        } else {
            tokio::fs::rename(&part, &dest).await?;
            self.host.received(&from, &dest);
        }
        {
            let mut s = self.session.lock().unwrap();
            if let Some(u) = s.as_mut().filter(|u| &u.id == sid) {
                u.done += 1;
                if u.done >= u.files.len() {
                    *s = None;
                }
            }
        }
        Ok(reply(StatusCode::OK, None))
    }

    async fn http(
        &self,
        peer: &Peer,
        method: Method,
        path: &str,
        body: Body,
        len: Option<u64>,
        timeout: Duration,
    ) -> Result<(StatusCode, Bytes)> {
        let addr = SocketAddr::new(peer.ip, peer.port);
        let tcp = tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(addr))
            .await
            .map_err(|_| anyhow!("{} did not answer", peer.alias))??;
        tcp.set_nodelay(true)?;
        let mut b = Request::builder().method(method).uri(path).header(HOST, addr.to_string()).header(CONTENT_TYPE, "application/json");
        if let Some(n) = len {
            b = b.header(CONTENT_LENGTH, n);
        }
        let req = b.body(body)?;
        if peer.https {
            let cfg = rustls::ClientConfig::builder_with_provider(self.provider.clone())
                .with_safe_default_protocol_versions()?
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(Pinned {
                    expect: peer.fingerprint.clone(),
                    algs: self.provider.signature_verification_algorithms,
                }))
                .with_client_auth_cert(vec![self.cert.clone()], PrivateKeyDer::Pkcs8(self.key.clone_key()))?;
            let tls = tokio_rustls::TlsConnector::from(Arc::new(cfg)).connect(ServerName::from(peer.ip), tcp).await?;
            exchange(TokioIo::new(tls), req, timeout).await
        } else {
            exchange(TokioIo::new(tcp), req, timeout).await
        }
    }

    async fn prepare(&self, peer: &Peer, files: HashMap<String, FileDto>) -> Result<Option<PrepareResp>> {
        let body = serde_json::to_vec(&json!({"info": self.info(None), "files": files}))?;
        let (status, bytes) = self
            .http(peer, Method::POST, &format!("{API}/prepare-upload"), json_body(body), None, Duration::from_secs(300))
            .await?;
        match status.as_u16() {
            200 => Ok(Some(serde_json::from_slice(&bytes).context("bad answer from LocalSend")?)),
            204 => Ok(None),
            401 => bail!("{} asks for a PIN; turn the PIN off in its LocalSend settings", peer.alias),
            403 => bail!("{} declined", peer.alias),
            409 => bail!("{} is busy with another transfer", peer.alias),
            s => bail!("{} answered {s}", peer.alias),
        }
    }

    pub async fn send_files(&self, peer: &Peer, paths: &[PathBuf]) -> Result<Vec<PathBuf>> {
        let mut files = HashMap::new();
        for (i, p) in paths.iter().enumerate() {
            let size = tokio::fs::metadata(p).await?.len();
            let name = p.file_name().context("no file name")?.to_string_lossy().into_owned();
            files.insert(
                i.to_string(),
                FileDto { id: i.to_string(), file_name: name, size, file_type: mime(p).into(), sha256: None, preview: None },
            );
        }
        let Some(resp) = self.prepare(peer, files).await? else { return Ok(Vec::new()) };
        let mut sent = Vec::new();
        for (id, tok) in &resp.files {
            let Some(path) = id.parse::<usize>().ok().and_then(|i| paths.get(i)) else { continue };
            let file = tokio::fs::File::open(path).await?;
            let len = file.metadata().await?.len();
            let stream = tokio_util::io::ReaderStream::with_capacity(file, 1 << 20).map(|r| r.map(Frame::data));
            let q = format!("{API}/upload?sessionId={}&fileId={}&token={}", encode(&resp.session_id), encode(id), encode(tok));
            let (status, _) = self
                .http(peer, Method::POST, &q, BodyExt::boxed(StreamBody::new(stream)), Some(len), Duration::from_secs(6 * 3600))
                .await?;
            if !status.is_success() {
                bail!("{} stopped the transfer ({status})", peer.alias);
            }
            sent.push(path.clone());
        }
        Ok(sent)
    }

    pub async fn send_text(&self, peer: &Peer, text: &str) -> Result<()> {
        let mut files = HashMap::new();
        files.insert(
            "0".to_string(),
            FileDto {
                id: "0".into(),
                file_name: "message.txt".into(),
                size: text.len() as u64,
                file_type: "text/plain".into(),
                sha256: None,
                preview: Some(text.to_string()),
            },
        );
        if let Some(resp) = self.prepare(peer, files).await? {
            if let Some(tok) = resp.files.get("0") {
                let q = format!("{API}/upload?sessionId={}&fileId=0&token={}", encode(&resp.session_id), encode(tok));
                let bytes = Bytes::from(text.as_bytes().to_vec());
                let len = bytes.len() as u64;
                self.http(peer, Method::POST, &q, json_body(bytes.to_vec()), Some(len), Duration::from_secs(60)).await?;
            }
        }
        Ok(())
    }
}

fn canonical(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(ip),
        v4 => v4,
    }
}

fn json_body(bytes: Vec<u8>) -> Body {
    Full::new(Bytes::from(bytes)).map_err(|never| match never {}).boxed()
}

async fn read_json<T: serde::de::DeserializeOwned>(body: Incoming, limit: usize) -> Result<T> {
    let bytes = Limited::new(body, limit).collect().await.map_err(|e| anyhow!("{e}"))?.to_bytes();
    Ok(serde_json::from_slice(&bytes)?)
}

async fn exchange<I>(io: I, req: Request<Body>, timeout: Duration) -> Result<(StatusCode, Bytes)>
where
    I: hyper::rt::Read + hyper::rt::Write + Unpin + Send + 'static,
{
    let (mut tx, conn) = hyper::client::conn::http1::handshake::<I, Body>(io).await?;
    tokio::spawn(async move {
        let _ = conn.await;
    });
    let resp = tokio::time::timeout(timeout, tx.send_request(req)).await.map_err(|_| anyhow!("timed out"))??;
    let status = resp.status();
    let bytes = Limited::new(resp.into_body(), 1 << 20).collect().await.map_err(|e| anyhow!("{e}"))?.to_bytes();
    Ok((status, bytes))
}
