use crate::proto::{self, ALPN, Msg};
use crate::store::{self, Config, Device};
use crate::phone::{self, ClipEvent, Session};
use crate::localsend::{self, LocalSend, Mode};
use crate::quickshare::{self, QuickShare};
use crate::clip::{self, Clip};
use crate::adb;
use anyhow::{Context, Result, anyhow, bail};
use iroh::endpoint::{Connection, QuicTransportConfig, VarInt, presets};
use iroh::protocol::{AcceptError, ProtocolHandler, Router};
use iroh::{Endpoint, EndpointAddr, EndpointId, SecretKey};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

const APK: &[u8] = include_bytes!("../assets/drop.apk");
const PAIR_WINDOW: Duration = Duration::from_secs(600);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(12);

#[derive(Deserialize, Debug)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Req {
    Status,
    Devices,
    Recent,
    Send { to: String, paths: Vec<PathBuf> },
    Get { from: String, path: String, dest: Option<PathBuf> },
    ClipGet { from: Option<String> },
    ClipSet { text: String, to: Option<String> },
    ClipImage { path: PathBuf, to: Option<String> },
    ClipSave { path: PathBuf },
    ClipSync { on: bool, device: Option<String> },
    Type { to: String, text: String },
    Key { to: String, keys: Vec<String> },
    PairStart,
    PairJoin { ticket: String },
    Rename { device: String, name: String },
    Forget { device: String },
    Localsend { mode: Option<Mode> },
    Quickshare { mode: Option<Mode> },
    Screenshots { on: Option<bool> },
    AndroidInstall { device: Option<String> },
    Stop,
}

pub struct State {
    ep: Endpoint,
    cfg: Mutex<Config>,
    pair: Mutex<Option<(String, Instant)>>,
    conns: tokio::sync::Mutex<HashMap<EndpointId, Connection>>,
    last_clip: Mutex<u64>,
    recent: Mutex<VecDeque<Value>>,
    adopt: tokio::sync::mpsc::UnboundedSender<Connection>,
    phones: Mutex<HashMap<String, Arc<Session>>>,
    clip_events: tokio::sync::mpsc::UnboundedSender<ClipEvent>,
    ls: Mutex<Option<Arc<LocalSend>>>,
    ls_error: Mutex<Option<String>>,
    qs: Mutex<Option<Arc<QuickShare>>>,
    qs_error: Mutex<Option<String>>,
    me: std::sync::Weak<State>,
}

struct LsHost(std::sync::Weak<State>);

impl localsend::Host for LsHost {
    fn alias(&self) -> String {
        self.0.upgrade().map(|s| s.cfg.lock().unwrap().name.clone()).unwrap_or_default()
    }

    fn inbox(&self) -> PathBuf {
        self.0.upgrade().map(|s| s.cfg.lock().unwrap().inbox.clone()).unwrap_or_else(|| store::data_dir().join("inbox"))
    }

    fn mode(&self) -> Mode {
        self.0.upgrade().map(|s| s.ls_mode()).unwrap_or(Mode::Off)
    }

    fn trusted(&self, fingerprint: &str) -> bool {
        self.0.upgrade().is_some_and(|s| s.cfg.lock().unwrap().localsend_trusted.iter().any(|f| f == fingerprint))
    }

    fn trust(&self, fingerprint: &str) {
        if let Some(s) = self.0.upgrade() {
            let _ = s.update_cfg(|c| {
                if !c.localsend_trusted.iter().any(|f| f == fingerprint) {
                    c.localsend_trusted.push(fingerprint.to_string());
                }
                Ok(())
            });
        }
    }

    fn received(&self, from: &str, path: &Path) {
        if let Some(s) = self.0.upgrade() {
            s.log("file_in", from, path.display().to_string());
            notify(from, &format!("Sent you {}", path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()), Note::File(path));
            eprintln!("got {} from {from} over LocalSend", path.display());
        }
    }

    fn text(&self, from: &str, text: &str) {
        let Some(s) = self.0.upgrade() else { return };
        s.log("clip_in", from, format!("{} chars", text.chars().count()));
        if clip::available() {
            let c = Clip::Text(text.to_string());
            tokio::task::spawn_blocking(move || clip::set(&c));
            notify(&format!("Clipboard from {from}"), &text.chars().take(200).collect::<String>(), Note::Plain);
        } else {
            let inbox = s.cfg.lock().unwrap().inbox.clone();
            let _ = std::fs::write(store::unique_path(&inbox, &format!("{from}.txt")), text);
        }
    }

    fn image(&self, from: &str, png: Vec<u8>) {
        let Some(s) = self.0.upgrade() else { return };
        let c = Clip::Image(png);
        s.log("clip_in", from, c.describe());
        if clip::available() {
            let from = from.to_string();
            tokio::task::spawn_blocking(move || {
                let Clip::Image(b) = &c else { return };
                let Some(png) = clip::to_png(b) else { return };
                let thumb = std::env::temp_dir().join("drop-clipboard.png");
                let shown = std::fs::write(&thumb, &png).is_ok();
                clip::set(&Clip::Image(png));
                notify(&format!("Clipboard from {from}"), "Image copied, ready to paste", if shown { Note::Image(&thumb) } else { Note::Plain });
            });
        } else if let Clip::Image(b) = c {
            let inbox = s.cfg.lock().unwrap().inbox.clone();
            let _ = std::fs::write(store::unique_path(&inbox, &format!("{from}.png")), b);
        }
    }
}

struct QsHost(LsHost);

impl quickshare::Host for QsHost {
    fn alias(&self) -> String {
        localsend::Host::alias(&self.0)
    }

    fn inbox(&self) -> PathBuf {
        localsend::Host::inbox(&self.0)
    }

    fn mode(&self) -> Mode {
        self.0.0.upgrade().map(|s| s.qs_mode()).unwrap_or(Mode::Off)
    }

    fn received(&self, from: &str, path: &Path) {
        localsend::Host::received(&self.0, from, path)
    }

    fn text(&self, from: &str, text: &str) {
        localsend::Host::text(&self.0, from, text)
    }
}

#[derive(Clone, PartialEq)]
enum Source {
    Local,
    Net(EndpointId),
    Phone(String),
}

#[derive(Clone)]
struct Handler(Arc<State>);

impl std::fmt::Debug for Handler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Handler")
    }
}

impl ProtocolHandler for Handler {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        serve_conn(self.0.clone(), conn).await;
        Ok(())
    }
}

async fn serve_conn(st: Arc<State>, conn: Connection) {
    let peer = conn.remote_id();
    if st.device_by_id(&peer).is_some() {
        let mut conns = st.conns.lock().await;
        if conns.get(&peer).is_none_or(|c| c.close_reason().is_some()) {
            conns.insert(peer, conn.clone());
        }
    }
    while let Ok((send, recv)) = conn.accept_bi().await {
        let st = st.clone();
        tokio::spawn(async move {
            if let Err(e) = st.serve_stream(peer, send, recv).await {
                eprintln!("stream from {}: {e:#}", peer.fmt_short());
            }
        });
    }
}

fn route(conn: &Connection) -> String {
    let paths = conn.paths();
    let Some(p) = paths.iter().find(|p| p.is_selected()) else { return "net".into() };
    match p.remote_addr() {
        iroh::TransportAddr::Ip(a) => {
            let kind = match a.ip() {
                std::net::IpAddr::V4(v4) if v4.octets()[0] == 100 && (v4.octets()[1] & 0xc0) == 64 => "tailscale",
                std::net::IpAddr::V4(v4) if v4.is_private() || v4.is_link_local() || v4.is_loopback() => "lan",
                std::net::IpAddr::V6(v6) if v6.is_loopback() || (v6.segments()[0] & 0xffc0) == 0xfe80 => "lan",
                _ => "direct",
            };
            format!("{kind} {}", a.ip())
        }
        _ => "relay".into(),
    }
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn load_key() -> Result<SecretKey> {
    let path = store::data_dir().join("key");
    if let Ok(bytes) = std::fs::read(&path) {
        let arr: [u8; 32] = bytes.as_slice().try_into().context("bad key file")?;
        return Ok(SecretKey::from_bytes(&arr));
    }
    let key = SecretKey::generate();
    std::fs::write(&path, key.to_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(key)
}

pub async fn run() -> Result<()> {
    let dir = store::data_dir();
    std::fs::create_dir_all(&dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    }
    store::menu_key()?;
    let sock = store::sock_path();
    if tokio::net::UnixStream::connect(&sock).await.is_ok() {
        eprintln!("drop is already running");
        return Ok(());
    }
    let _ = std::fs::remove_file(&sock);

    let cfg = Config::load_or_init()?;
    std::fs::create_dir_all(&cfg.inbox)?;
    let ep = Endpoint::builder(presets::N0)
        .secret_key(load_key()?)
        .alpns(vec![ALPN.to_vec()])
        .transport_config(
            QuicTransportConfig::builder()
                .stream_receive_window(VarInt::from_u32(16 << 20))
                .receive_window(VarInt::from_u32(32 << 20))
                .send_window(32 << 20)
                .build(),
        )
        .bind()
        .await
        .map_err(|e| anyhow!("{e}"))?;
    match iroh_mdns_address_lookup::MdnsAddressLookup::builder()
        .service_name("drop")
        .discovery_cadence(Duration::from_secs(30))
        .build(ep.id()) {
        Ok(mdns) => match ep.address_lookup() {
            Ok(l) => l.add(mdns),
            Err(e) => eprintln!("mdns: {e}"),
        },
        Err(e) => eprintln!("mdns: {e}"),
    }

    let (adopt_tx, mut adopt_rx) = tokio::sync::mpsc::unbounded_channel();
    let (clip_tx, mut clip_rx) = tokio::sync::mpsc::unbounded_channel::<ClipEvent>();
    let st = Arc::new_cyclic(|me| State {
        ep: ep.clone(),
        cfg: Mutex::new(cfg),
        pair: Mutex::new(None),
        conns: Default::default(),
        last_clip: Mutex::new(clip::get().map(|c| c.hash()).unwrap_or(0)),
        recent: Mutex::new(VecDeque::new()),
        adopt: adopt_tx,
        phones: Mutex::new(HashMap::new()),
        clip_events: clip_tx,
        ls: Mutex::new(None),
        ls_error: Mutex::new(None),
        qs: Mutex::new(None),
        qs_error: Mutex::new(None),
        me: me.clone(),
    });
    {
        let st = st.clone();
        tokio::spawn(async move {
            while let Some(ev) = clip_rx.recv().await {
                st.spread(Source::Phone(ev.serial), ev.clip).await;
            }
        });
    }
    {
        let st = st.clone();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(phone::track(tx));
        tokio::spawn(async move {
            while rx.recv().await.is_some() {
                tokio::time::sleep(Duration::from_millis(400)).await;
                while rx.try_recv().is_ok() {}
                st.refresh_phones().await;
            }
        });
    }
    {
        let st = st.clone();
        tokio::spawn(async move {
            while let Some(c) = adopt_rx.recv().await {
                tokio::spawn(serve_conn(st.clone(), c));
            }
        });
    }
    let router = Router::builder(ep).accept(ALPN, Handler(st.clone())).spawn();
    eprintln!("drop {} as \"{}\"", st.ep.id(), st.cfg.lock().unwrap().name);

    if clip::available() {
        tokio::spawn(watch_clipboard(st.clone()));
    }
    #[cfg(target_os = "macos")]
    tokio::spawn(watch_airdrop(st.clone()));
    #[cfg(target_os = "macos")]
    tokio::spawn(watch_screenshots(st.clone()));
    st.start_localsend().await;
    st.start_quickshare().await;

    let listener = tokio::net::UnixListener::bind(&sock)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&sock, std::fs::Permissions::from_mode(0o600))?;
    }
    let (stop_tx, mut stop_rx) = tokio::sync::mpsc::channel::<()>(1);
    #[cfg(unix)]
    {
        let stop = stop_tx.clone();
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::spawn(async move {
            term.recv().await;
            let _ = stop.send(()).await;
        });
    }
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let Ok((stream, _)) = accepted else { continue };
                let st = st.clone();
                let stop = stop_tx.clone();
                tokio::spawn(async move { let _ = serve_local(st, stream, stop).await; });
            }
            _ = stop_rx.recv() => break,
            _ = tokio::signal::ctrl_c() => break,
        }
    }
    let _ = std::fs::remove_file(&sock);
    for sess in st.phones.lock().unwrap().values() {
        sess.kill();
    }
    let _ = tokio::time::timeout(Duration::from_secs(2), router.shutdown()).await;
    Ok(())
}

async fn serve_local(
    st: Arc<State>,
    stream: tokio::net::UnixStream,
    stop: tokio::sync::mpsc::Sender<()>,
) -> Result<()> {
    let (r, mut w) = stream.into_split();
    let mut lines = BufReader::new(r).lines();
    while let Some(line) = lines.next_line().await? {
        let reply = match serde_json::from_str::<Req>(&line) {
            Ok(Req::Stop) => {
                let _ = stop.send(()).await;
                json!({"ok": true})
            }
            Ok(req) => match st.handle(req).await {
                Ok(mut v) => {
                    if let Value::Object(m) = &mut v {
                        m.insert("ok".into(), true.into());
                    }
                    v
                }
                Err(e) => json!({"ok": false, "error": format!("{e:#}")}),
            },
            Err(e) => json!({"ok": false, "error": format!("bad request: {e}")}),
        };
        let mut out = serde_json::to_vec(&reply)?;
        out.push(b'\n');
        w.write_all(&out).await?;
    }
    Ok(())
}

async fn watch_clipboard(st: Arc<State>) {
    let mut count = clip::change_count();
    let mut tick = tokio::time::interval(Duration::from_millis(350));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
        let c = clip::change_count();
        if c == count {
            continue;
        }
        count = c;
        if !st.cfg.lock().unwrap().clip {
            continue;
        }
        let Ok(Some(c)) = tokio::task::spawn_blocking(clip::get).await else { continue };
        st.spread(Source::Local, c).await;
    }
}

#[cfg(target_os = "macos")]
fn from_airdrop(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(p) = std::ffi::CString::new(path.as_os_str().as_bytes()) else { return false };
    let mut buf = [0u8; 256];
    let n = unsafe {
        libc::getxattr(p.as_ptr(), c"com.apple.quarantine".as_ptr(), buf.as_mut_ptr().cast(), buf.len(), 0, 0)
    };
    n > 0 && buf[..n as usize].windows(10).any(|w| w == b";sharingd;")
}

#[cfg(target_os = "macos")]
async fn watch_airdrop(st: Arc<State>) {
    let dir = store::home().join("Downloads");
    let started = SystemTime::now();
    let mut checked = std::collections::HashSet::new();
    let mut last = None;
    loop {
        tokio::time::sleep(Duration::from_secs(3)).await;
        let Ok(m) = tokio::fs::metadata(&dir).await.and_then(|m| m.modified()) else { continue };
        if last == Some(m) {
            continue;
        }
        last = Some(m);
        let Ok(mut rd) = tokio::fs::read_dir(&dir).await else { continue };
        while let Ok(Some(e)) = rd.next_entry().await {
            let name = e.file_name();
            if name.to_string_lossy().starts_with('.') || checked.contains(&name) {
                continue;
            }
            let Ok(meta) = e.metadata().await else { continue };
            if meta.created().or_else(|_| meta.modified()).is_ok_and(|t| t < started) {
                checked.insert(name);
                continue;
            }
            let path = e.path();
            if from_airdrop(&path) {
                st.log("file_in", "AirDrop", path.display().to_string());
            }
            checked.insert(name);
        }
    }
}

#[cfg(target_os = "macos")]
fn screenshot_dir() -> Option<PathBuf> {
    let out = std::process::Command::new("defaults").args(["read", "com.apple.screencapture", "location"]).output().ok();
    let set = out.filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    let dir = match set.filter(|s| !s.is_empty()) {
        Some(s) if s == "~" || s.starts_with("~/") => store::home().join(s.trim_start_matches('~').trim_start_matches('/')),
        Some(s) => PathBuf::from(s),
        None => store::home().join("Desktop"),
    };
    Some(dir)
}

#[cfg(not(target_os = "macos"))]
fn screenshot_dir() -> Option<PathBuf> {
    None
}

#[cfg(target_os = "macos")]
fn is_screenshot(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(p) = std::ffi::CString::new(path.as_os_str().as_bytes()) else { return false };
    let n = unsafe {
        libc::getxattr(p.as_ptr(), c"com.apple.metadata:kMDItemIsScreenCapture".as_ptr(), std::ptr::null_mut(), 0, 0, 0)
    };
    n > 0
}

#[cfg(target_os = "macos")]
async fn watch_screenshots(st: Arc<State>) {
    let started = SystemTime::now();
    let mut checked = std::collections::HashSet::new();
    let mut last = None;
    let mut dir = screenshot_dir();
    let mut dir_checked = Instant::now();
    loop {
        tokio::time::sleep(Duration::from_secs(2)).await;
        if !st.screenshots_on() {
            continue;
        }
        if dir_checked.elapsed() > Duration::from_secs(120) {
            dir = screenshot_dir();
            dir_checked = Instant::now();
        }
        let Some(d) = dir.clone() else { continue };
        let Ok(m) = tokio::fs::metadata(&d).await.and_then(|m| m.modified()) else { continue };
        if last == Some((d.clone(), m)) {
            continue;
        }
        last = Some((d.clone(), m));
        let Ok(mut rd) = tokio::fs::read_dir(&d).await else { continue };
        while let Ok(Some(e)) = rd.next_entry().await {
            let path = e.path();
            if path.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.')) || checked.contains(&path) {
                continue;
            }
            checked.insert(path.clone());
            let Ok(meta) = e.metadata().await else { continue };
            if !meta.is_file() || meta.created().or_else(|_| meta.modified()).is_ok_and(|t| t < started) {
                continue;
            }
            if !path.extension().is_some_and(|x| x.eq_ignore_ascii_case("png")) || !is_screenshot(&path) {
                continue;
            }
            let Ok(bytes) = tokio::fs::read(&path).await else { continue };
            if bytes.len() as u64 > clip::MAX_IMAGE || !clip::is_png(&bytes) {
                continue;
            }
            let c = Clip::Image(bytes);
            let _ = tokio::task::spawn_blocking({
                let c = c.clone();
                move || clip::set(&c)
            })
            .await;
            st.spread(Source::Local, c).await;
        }
    }
}

impl State {
    async fn spread(&self, src: Source, clip: Clip) {
        if clip.is_empty() || !self.cfg.lock().unwrap().clip {
            return;
        }
        let clip = match clip {
            Clip::Image(b) if !clip::is_png(&b) => match tokio::task::spawn_blocking(move || clip::to_png(&b)).await {
                Ok(Some(png)) => Clip::Image(png),
                _ => return,
            },
            c => c,
        };
        let h = clip.hash();
        {
            let mut last = self.last_clip.lock().unwrap();
            if *last == h {
                return;
            }
            *last = h;
        }
        let (devices, from) = {
            let cfg = self.cfg.lock().unwrap();
            let from = match &src {
                Source::Local => cfg.name.clone(),
                Source::Net(id) => {
                    let ids = id.to_string();
                    cfg.devices.iter().find(|d| d.id.as_deref() == Some(&ids)).map(|d| d.name.clone()).unwrap_or_default()
                }
                Source::Phone(serial) => match cfg.devices.iter().find(|d| d.serial.as_deref() == Some(serial)) {
                    Some(d) if d.clip => d.name.clone(),
                    _ => return,
                },
            };
            (cfg.devices.clone(), from)
        };
        if src != Source::Local && clip::available() {
            let c = clip.clone();
            let _ = tokio::task::spawn_blocking(move || clip::set(&c)).await;
        }
        if src != Source::Local {
            self.log("clip_in", &from, clip.describe());
        }
        let Some(me) = self.me.upgrade() else { return };
        let clip = Arc::new(clip);
        let to_net = !matches!(src, Source::Net(_));
        for d in devices.into_iter().filter(|d| d.clip) {
            if let (true, Some(id)) = (to_net, d.id.as_deref().and_then(|i| i.parse::<EndpointId>().ok())) {
                if src != Source::Net(id) {
                    let (me, clip, d) = (me.clone(), clip.clone(), d.clone());
                    tokio::spawn(async move {
                        if let Err(e) = me.clip_to(&d, &clip).await {
                            eprintln!("clipboard to {}: {e:#}", d.name);
                        }
                    });
                }
            }
            if let Some(serial) = d.serial.as_ref().filter(|s| src != Source::Phone((*s).clone())) {
                if let Some(sess) = self.session(serial) {
                    let clip = clip.clone();
                    tokio::spawn(async move {
                        if let Err(e) = sess.set(&clip).await {
                            eprintln!("clipboard to phone {}: {e:#}", sess.serial);
                        }
                    });
                }
            }
        }
    }

    fn screenshots_on(&self) -> bool {
        self.cfg.lock().unwrap().screenshots.unwrap_or(cfg!(target_os = "macos"))
    }

    fn ls_mode(&self) -> Mode {
        self.cfg.lock().unwrap().localsend.unwrap_or_else(Mode::platform_default)
    }

    fn ls(&self) -> Option<Arc<LocalSend>> {
        if self.ls_mode() == Mode::Off {
            return None;
        }
        self.ls.lock().unwrap().clone()
    }

    async fn start_localsend(&self) {
        if self.ls_mode() == Mode::Off || self.ls.lock().unwrap().is_some() {
            return;
        }
        match LocalSend::start(Arc::new(LsHost(self.me.clone()))).await {
            Ok(ls) => {
                eprintln!("localsend on port {}", localsend::PORT);
                *self.ls_error.lock().unwrap() = None;
                *self.ls.lock().unwrap() = Some(ls);
            }
            Err(e) => {
                eprintln!("localsend: {e:#}");
                *self.ls_error.lock().unwrap() = Some(format!("{e:#}"));
            }
        }
    }

    fn qs_mode(&self) -> Mode {
        self.cfg.lock().unwrap().quickshare.unwrap_or_else(Mode::platform_default)
    }

    async fn start_quickshare(&self) {
        if self.qs_mode() == Mode::Off || self.qs.lock().unwrap().is_some() {
            return;
        }
        match QuickShare::start(Arc::new(QsHost(LsHost(self.me.clone())))).await {
            Ok(qs) => {
                eprintln!("quickshare on port {}", qs.port);
                *self.qs_error.lock().unwrap() = None;
                *self.qs.lock().unwrap() = Some(qs);
            }
            Err(e) => {
                eprintln!("quickshare: {e:#}");
                *self.qs_error.lock().unwrap() = Some(format!("{e:#}"));
            }
        }
    }

    fn ls_peer(&self, q: &str) -> Option<(Arc<LocalSend>, localsend::Peer)> {
        let ls = self.ls()?;
        let peer = ls.find(q)?;
        Some((ls, peer))
    }

    async fn send_localsend(&self, ls: &LocalSend, peer: &localsend::Peer, paths: &[PathBuf]) -> Result<Value> {
        for p in paths {
            if !p.is_file() {
                bail!("{} is not a file", p.display());
            }
        }
        let started = Instant::now();
        let sent = ls.send_files(peer, paths).await?;
        let secs = started.elapsed().as_secs_f64();
        let mut results = Vec::new();
        for p in &sent {
            let bytes = tokio::fs::metadata(p).await.map(|m| m.len()).unwrap_or(0);
            self.log("file_out", &peer.alias, p.display().to_string());
            results.push(json!({"file": p, "bytes": bytes, "secs": secs / sent.len() as f64, "via": format!("localsend {}", peer.ip), "dest": ""}));
        }
        Ok(json!({"device": peer.alias, "sent": results}))
    }

    fn qs_peer(&self, q: &str) -> Option<(Arc<QuickShare>, quickshare::Peer)> {
        if self.qs_mode() == Mode::Off {
            return None;
        }
        let qs = self.qs.lock().unwrap().clone()?;
        let peer = qs.find(q)?;
        Some((qs, peer))
    }

    async fn send_quickshare(&self, qs: &QuickShare, peer: &quickshare::Peer, paths: &[PathBuf]) -> Result<Value> {
        for p in paths {
            if !p.is_file() {
                bail!("{} is not a file", p.display());
            }
        }
        let started = Instant::now();
        qs.send_files(peer, paths).await?;
        let secs = started.elapsed().as_secs_f64();
        let mut results = Vec::new();
        for p in paths {
            let bytes = tokio::fs::metadata(p).await.map(|m| m.len()).unwrap_or(0);
            self.log("file_out", &peer.name, p.display().to_string());
            results.push(json!({"file": p, "bytes": bytes, "secs": secs / paths.len() as f64, "via": format!("quickshare {}", peer.addr.ip()), "dest": ""}));
        }
        Ok(json!({"device": peer.name, "sent": results}))
    }

    fn session(&self, serial: &str) -> Option<Arc<Session>> {
        self.phones.lock().unwrap().get(serial).filter(|s| s.alive()).cloned()
    }

    async fn refresh_phones(&self) {
        let phones = self.sync_phones().await;
        let stale: Vec<Arc<Session>> = {
            let mut map = self.phones.lock().unwrap();
            let gone: Vec<String> = map
                .iter()
                .filter(|(_, s)| !s.alive() || !phones.iter().any(|p| p.transport == s.transport))
                .map(|(k, _)| k.clone())
                .collect();
            gone.iter().filter_map(|k| map.remove(k)).collect()
        };
        for s in stale {
            s.kill();
        }
        let mut started = std::collections::HashSet::new();
        for p in phones {
            if self.session(&p.serial).is_some() || !started.insert(p.serial.clone()) {
                continue;
            }
            match Session::start(&p.serial, &p.transport, self.clip_events.clone()).await {
                Ok(sess) => {
                    eprintln!("helper running on {} via {}", p.name, p.transport);
                    self.phones.lock().unwrap().insert(p.serial.clone(), sess);
                }
                Err(e) => {
                    started.remove(&p.serial);
                    eprintln!("helper on {} via {}: {e:#}", p.name, p.transport);
                }
            }
        }
    }

    fn device_by_id(&self, id: &EndpointId) -> Option<Device> {
        let s = id.to_string();
        self.cfg.lock().unwrap().devices.iter().find(|d| d.id.as_deref() == Some(&s)).cloned()
    }

    fn log(&self, kind: &str, device: &str, detail: impl Into<String>) {
        let mut r = self.recent.lock().unwrap();
        r.push_front(json!({"at": now(), "kind": kind, "device": device, "detail": detail.into()}));
        r.truncate(50);
    }

    fn update_cfg<T>(&self, f: impl FnOnce(&mut Config) -> Result<T>) -> Result<T> {
        let mut cfg = self.cfg.lock().unwrap();
        let out = f(&mut cfg)?;
        cfg.save()?;
        Ok(out)
    }

    async fn conn(&self, id: EndpointId) -> Result<Connection> {
        let mut conns = self.conns.lock().await;
        if let Some(c) = conns.get(&id).filter(|c| c.close_reason().is_none()) {
            return Ok(c.clone());
        }
        let c = tokio::time::timeout(CONNECT_TIMEOUT, self.ep.connect(EndpointAddr::from(id), ALPN))
            .await
            .map_err(|_| anyhow!("device did not answer (is drop running there?)"))?
            .map_err(|e| anyhow!("connect: {e}"))?;
        conns.insert(id, c.clone());
        let _ = self.adopt.send(c.clone());
        Ok(c)
    }

    async fn request(&self, id: EndpointId, msg: &Msg) -> Result<Msg> {
        let conn = self.conn(id).await?;
        let (mut s, mut r) = conn.open_bi().await?;
        proto::write(&mut s, msg).await?;
        s.finish()?;
        let reply: Msg = proto::read(&mut r).await?;
        if let Msg::Err { error } = reply {
            bail!(error);
        }
        Ok(reply)
    }

    async fn clip_to(&self, d: &Device, clip: &Clip) -> Result<()> {
        let id: EndpointId = d.id.as_deref().context("no network id")?.parse()?;
        let bytes = match clip {
            Clip::Text(text) => {
                self.request(id, &Msg::Clip { text: text.clone() }).await?;
                return Ok(());
            }
            Clip::Image(b) => b,
        };
        let conn = self.conn(id).await?;
        let (mut s, mut r) = conn.open_bi().await?;
        proto::write(&mut s, &Msg::ClipImage { size: bytes.len() as u64 }).await?;
        s.write_all(bytes).await?;
        s.finish()?;
        match proto::read::<_, Msg>(&mut r).await? {
            Msg::Err { error } if error.starts_with("unexpected") => bail!("{} runs an older drop without image clipboard", d.name),
            Msg::Err { error } => bail!(error),
            _ => Ok(()),
        }
    }

    async fn serve_stream(
        &self,
        peer: EndpointId,
        mut send: iroh::endpoint::SendStream,
        mut recv: iroh::endpoint::RecvStream,
    ) -> Result<()> {
        let msg: Msg = proto::read(&mut recv).await?;
        let known = self.device_by_id(&peer);
        let reply = match (msg, known) {
            (Msg::Pair { code, name }, _) => self.accept_pair(peer, &code, &name),
            (_, None) => Err(anyhow!("not paired")),
            (Msg::Clip { text }, Some(dev)) => {
                if dev.clip {
                    self.spread(Source::Net(peer), Clip::Text(text)).await;
                }
                Ok(Msg::Ok { name: None, detail: None })
            }
            (Msg::ClipImage { size }, Some(dev)) => {
                if size > clip::MAX_IMAGE {
                    let _ = recv.stop(1u32.into());
                    Err(anyhow!("image is too large"))
                } else {
                    let mut buf = Vec::with_capacity(size as usize);
                    (&mut recv).take(size).read_to_end(&mut buf).await?;
                    if buf.len() as u64 != size {
                        bail!("image cut short");
                    }
                    if dev.clip {
                        self.spread(Source::Net(peer), Clip::Image(buf)).await;
                    }
                    Ok(Msg::Ok { name: None, detail: None })
                }
            }
            (Msg::File { name, size }, Some(dev)) => {
                proto::write(&mut send, &Msg::Ok { name: None, detail: None }).await?;
                let started = Instant::now();
                match self.receive_file(&mut recv, &name, size).await {
                    Ok(path) => {
                        let secs = started.elapsed().as_secs_f64();
                        self.log("file_in", &dev.name, path.display().to_string());
                        notify(&dev.name, &format!("Sent you {name}"), Note::File(&path));
                        eprintln!("got {} ({size} bytes, {secs:.1}s) from {}", path.display(), dev.name);
                        Ok(Msg::Ok { name: None, detail: Some(path.display().to_string()) })
                    }
                    Err(e) => {
                        let _ = recv.stop(1u32.into());
                        Err(e)
                    }
                }
            }
            (other, _) => Err(anyhow!("unexpected {other:?}")),
        };
        let reply = reply.unwrap_or_else(|e| Msg::Err { error: format!("{e:#}") });
        proto::write(&mut send, &reply).await?;
        send.finish()?;
        let _ = tokio::time::timeout(Duration::from_secs(5), send.stopped()).await;
        Ok(())
    }

    async fn receive_file(&self, recv: &mut iroh::endpoint::RecvStream, name: &str, size: u64) -> Result<PathBuf> {
        let inbox = self.cfg.lock().unwrap().inbox.clone();
        tokio::fs::create_dir_all(&inbox).await?;
        let (dest, part, file) = {
            let name = name.to_string();
            tokio::task::spawn_blocking(move || store::reserve(&inbox, &name)).await??
        };
        let mut out = tokio::io::BufWriter::with_capacity(1 << 20, tokio::fs::File::from_std(file));
        let got = tokio::io::copy(&mut recv.take(size), &mut out).await;
        let flushed = out.flush().await;
        match (got, flushed) {
            (Ok(n), Ok(())) if n == size => {
                tokio::fs::rename(&part, &dest).await?;
                Ok(dest)
            }
            (got, flushed) => {
                let _ = tokio::fs::remove_file(&part).await;
                got?;
                flushed?;
                bail!("transfer cut short")
            }
        }
    }

    fn accept_pair(&self, peer: EndpointId, code: &str, name: &str) -> Result<Msg> {
        {
            let mut pending = self.pair.lock().unwrap();
            match pending.as_ref() {
                Some((c, at)) if c == code && at.elapsed() < PAIR_WINDOW => *pending = None,
                _ => bail!("pairing code is wrong or expired"),
            }
        }
        let id = peer.to_string();
        let me = self.update_cfg(|cfg| {
            match cfg.devices.iter_mut().find(|d| d.id.as_deref() == Some(&id)) {
                Some(d) => d.name = name.to_string(),
                None => cfg.devices.push(Device { name: name.to_string(), id: Some(id.clone()), serial: None, clip: true }),
            }
            Ok(cfg.name.clone())
        })?;
        self.log("paired", name, "");
        notify("Paired", &format!("{name} can now send to this device"), Note::Plain);
        Ok(Msg::Ok { name: Some(me), detail: None })
    }

    async fn sync_phones(&self) -> Vec<adb::Phone> {
        let phones = adb::phones().await;
        let _ = self.update_cfg(|cfg| {
            for p in &phones {
                if !cfg.devices.iter().any(|d| d.serial.as_deref() == Some(&p.serial)) {
                    cfg.devices.push(Device { name: p.name.clone(), id: None, serial: Some(p.serial.clone()), clip: true });
                }
            }
            Ok(())
        });
        phones
    }

    async fn resolve(&self, query: &str) -> Result<(Device, Option<adb::Phone>)> {
        let phones = self.sync_phones().await;
        let dev = {
            let cfg = self.cfg.lock().unwrap();
            cfg.devices[cfg.find(query)?].clone()
        };
        let phone = dev.serial.as_ref().and_then(|s| phones.into_iter().find(|p| &p.serial == s));
        Ok((dev, phone))
    }

    async fn handle(&self, req: Req) -> Result<Value> {
        match req {
            Req::Status => {
                let cfg = self.cfg.lock().unwrap().clone();
                Ok(json!({
                    "name": cfg.name,
                    "id": self.ep.id().to_string(),
                    "clip": cfg.clip,
                    "clip_here": clip::available(),
                    "inbox": cfg.inbox,
                    "adb": adb::adb_path(),
                    "addr": serde_json::to_value(self.ep.addr()).ok(),
                    "devices": cfg.devices.len(),
                }))
            }
            Req::Devices => {
                let phones = self.sync_phones().await;
                let conns = self.conns.lock().await;
                let cfg = self.cfg.lock().unwrap().clone();
                let list: Vec<Value> = cfg
                    .devices
                    .iter()
                    .map(|d| {
                        let mut via = Vec::new();
                        if let Some(s) = &d.serial {
                            for p in phones.iter().filter(|p| &p.serial == s) {
                                via.push(if p.usb { "usb".to_string() } else { format!("adb {}", p.transport) });
                            }
                        }
                        if let Some(id) = d.id.as_deref().and_then(|i| i.parse::<EndpointId>().ok()) {
                            match conns.get(&id).filter(|c| c.close_reason().is_none()) {
                                Some(c) => via.push(route(c)),
                                None => via.push("net".into()),
                            }
                        }
                        let live = d.serial.as_deref().and_then(|s| self.session(s)).is_some();
                        json!({"name": d.name, "id": d.id, "serial": d.serial, "clip": d.clip, "helper": live, "via": via})
                    })
                    .collect();
                let mut list = list;
                if let Some(ls) = self.ls() {
                    ls.nudge();
                    for p in ls.peers() {
                        merge_row(&mut list, &p.alias, format!("localsend {}", p.ip), "localsend", json!(p.fingerprint), p.device_type.as_deref());
                    }
                }
                if let Some(qs) = self.qs.lock().unwrap().clone().filter(|_| self.qs_mode() != Mode::Off) {
                    for p in qs.peers() {
                        let kind = match p.device_type { 1 | 5 => "mobile", 2 => "tablet", _ => "desktop" };
                        merge_row(&mut list, &p.name, format!("quickshare {}", p.addr.ip()), "quickshare", json!(p.addr.to_string()), Some(kind));
                    }
                }
                Ok(json!({"devices": list}))
            }
            Req::Recent => Ok(json!({"recent": self.recent.lock().unwrap().iter().cloned().collect::<Vec<_>>()})),
            Req::Send { to, paths } => {
                let (dev, phone) = match self.resolve(&to).await {
                    Ok(r) => r,
                    Err(e) => {
                        if let Some((ls, peer)) = self.ls_peer(&to) {
                            return self.send_localsend(&ls, &peer, &paths).await;
                        }
                        if let Some((qs, peer)) = self.qs_peer(&to) {
                            return self.send_quickshare(&qs, &peer, &paths).await;
                        }
                        return Err(e);
                    }
                };
                let direct = phone.is_some() || dev.id.is_some();
                if !direct {
                    if let Some((ls, peer)) = self.ls_peer(&dev.name) {
                        return self.send_localsend(&ls, &peer, &paths).await;
                    }
                    if let Some((qs, peer)) = self.qs_peer(&dev.name) {
                        return self.send_quickshare(&qs, &peer, &paths).await;
                    }
                }
                let mut results = Vec::new();
                for path in paths.iter() {
                    let started = Instant::now();
                    let size = tokio::fs::metadata(path).await.with_context(|| path.display().to_string())?.len();
                    if !path.is_file() {
                        bail!("{} is not a file", path.display());
                    }
                    let (via, dest) = match self.send_one(&dev, phone.as_ref(), path, size).await {
                        Ok(r) => r,
                        Err(e) if results.is_empty() => {
                            if let Some((ls, peer)) = self.ls_peer(&dev.name) {
                                return self.send_localsend(&ls, &peer, &paths).await;
                            }
                            if let Some((qs, peer)) = self.qs_peer(&dev.name) {
                                return self.send_quickshare(&qs, &peer, &paths).await;
                            }
                            return Err(e);
                        }
                        Err(e) => return Err(e),
                    };
                    let secs = started.elapsed().as_secs_f64();
                    self.log("file_out", &dev.name, path.display().to_string());
                    results.push(json!({"file": path, "bytes": size, "secs": secs, "via": via, "dest": dest}));
                }
                Ok(json!({"device": dev.name, "sent": results}))
            }
            Req::Get { from, path, dest } => {
                let (dev, phone) = self.resolve(&from).await?;
                let phone = phone.context(format!("{} is not connected over adb", dev.name))?;
                let dest = match dest {
                    Some(d) => d,
                    None => {
                        let inbox = self.cfg.lock().unwrap().inbox.clone();
                        store::unique_path(&inbox, path.rsplit('/').next().unwrap_or("file"))
                    }
                };
                adb::pull(&phone.transport, &path, &dest).await?;
                self.log("file_in", &dev.name, dest.display().to_string());
                Ok(json!({"device": dev.name, "saved": dest}))
            }
            Req::ClipGet { from: None } => Ok(match tokio::task::spawn_blocking(clip::get).await? {
                Some(Clip::Image(b)) => json!({"text": null, "image": b.len()}),
                Some(Clip::Text(t)) => json!({"text": t}),
                None => json!({"text": null}),
            }),
            Req::ClipSave { path } => match tokio::task::spawn_blocking(clip::get).await? {
                Some(Clip::Image(b)) => {
                    tokio::fs::write(&path, &b).await?;
                    Ok(json!({"saved": path, "bytes": b.len()}))
                }
                _ => bail!("there is no image on the clipboard"),
            },
            Req::ClipImage { path, to } => {
                let bytes = tokio::fs::read(&path).await.with_context(|| path.display().to_string())?;
                if bytes.len() as u64 > clip::MAX_IMAGE {
                    bail!("image is too large");
                }
                let png = tokio::task::spawn_blocking(move || clip::to_png(&bytes)).await?.context("that file is not an image drop can read")?;
                let c = Clip::Image(png);
                let Some(to) = to else {
                    if !clip::available() {
                        bail!("no clipboard on this device");
                    }
                    tokio::task::spawn_blocking(move || clip::set(&c)).await?;
                    return Ok(json!({}));
                };
                let (dev, _) = self.resolve(&to).await?;
                if let Some(sess) = dev.serial.as_deref().and_then(|s| self.session(s)) {
                    sess.set(&c).await?;
                } else if dev.id.is_some() {
                    self.clip_to(&dev, &c).await?;
                } else {
                    bail!("{}'s clipboard is not reachable right now", dev.name);
                }
                Ok(json!({"device": dev.name}))
            }
            Req::AndroidInstall { device } => {
                let phone = match device {
                    Some(q) => {
                        let (dev, phone) = self.resolve(&q).await?;
                        phone.context(format!("{} is not connected over adb", dev.name))?
                    }
                    None => {
                        let mut phones = self.sync_phones().await;
                        match phones.len() {
                            0 => bail!("no phone is connected over adb"),
                            1 => phones.remove(0),
                            _ => bail!("more than one phone is connected, name one: drop android install <device>"),
                        }
                    }
                };
                let apk = store::data_dir().join("drop.apk");
                tokio::fs::write(&apk, APK).await?;
                let r = adb::install(&phone.transport, &apk).await;
                let _ = tokio::fs::remove_file(&apk).await;
                r?;
                Ok(json!({"device": phone.name}))
            }
            Req::Screenshots { on } => {
                if let Some(on) = on {
                    self.update_cfg(|c| {
                        c.screenshots = Some(on);
                        Ok(())
                    })?;
                }
                Ok(json!({"screenshots": self.screenshots_on(), "dir": screenshot_dir()}))
            }
            Req::ClipGet { from: Some(q) } => {
                let (dev, _) = self.resolve(&q).await?;
                let sess = dev.serial.as_deref().and_then(|s| self.session(s));
                let sess = sess.context(format!("{}'s clipboard is not reachable (needs adb)", dev.name))?;
                Ok(json!({"device": dev.name, "text": sess.get_clip().await?}))
            }
            Req::ClipSet { text, to: None } => {
                if !clip::available() {
                    bail!("no clipboard on this device");
                }
                tokio::task::spawn_blocking(move || clip::set(&Clip::Text(text))).await?;
                Ok(json!({}))
            }
            Req::ClipSet { text, to: Some(to) } => {
                let dev = match self.resolve(&to).await {
                    Ok((d, _)) => d,
                    Err(e) => {
                        if let Some((ls, peer)) = self.ls_peer(&to) {
                            ls.send_text(&peer, &text).await?;
                            return Ok(json!({"device": peer.alias}));
                        }
                        if let Some((qs, peer)) = self.qs_peer(&to) {
                            qs.send_text(&peer, &text).await?;
                            return Ok(json!({"device": peer.name}));
                        }
                        return Err(e);
                    }
                };
                let c = Clip::Text(text.clone());
                let direct = if let Some(sess) = dev.serial.as_deref().and_then(|s| self.session(s)) {
                    sess.set(&c).await
                } else if dev.id.is_some() {
                    self.clip_to(&dev, &c).await
                } else {
                    Err(anyhow!("{}'s clipboard is not reachable right now", dev.name))
                };
                if let Err(e) = direct {
                    if let Some((ls, peer)) = self.ls_peer(&dev.name) {
                        ls.send_text(&peer, &text).await?;
                    } else if let Some((qs, peer)) = self.qs_peer(&dev.name) {
                        qs.send_text(&peer, &text).await?;
                    } else {
                        return Err(e);
                    }
                }
                Ok(json!({"device": dev.name}))
            }
            Req::ClipSync { on, device: None } => {
                self.update_cfg(|c| {
                    c.clip = on;
                    Ok(())
                })?;
                Ok(json!({"clip": on}))
            }
            Req::ClipSync { on, device: Some(q) } => {
                let name = self.update_cfg(|c| {
                    let i = c.find(&q)?;
                    c.devices[i].clip = on;
                    Ok(c.devices[i].name.clone())
                })?;
                Ok(json!({"device": name, "clip": on}))
            }
            Req::Type { to, text } => {
                let (dev, phone) = self.resolve(&to).await?;
                if let Some(sess) = dev.serial.as_deref().and_then(|s| self.session(s)) {
                    sess.type_text(&text).await?;
                } else {
                    let phone = phone.context(format!("{} is not connected over adb", dev.name))?;
                    adb::type_text(&phone.transport, &text).await?;
                }
                Ok(json!({"device": dev.name}))
            }
            Req::Key { to, keys } => {
                let (dev, phone) = self.resolve(&to).await?;
                if let Some(sess) = dev.serial.as_deref().and_then(|s| self.session(s)) {
                    let codes = keys.iter().map(|k| phone::keycode(k)).collect::<Result<Vec<_>>>()?;
                    sess.keys(&codes).await?;
                } else {
                    let phone = phone.context(format!("{} is not connected over adb", dev.name))?;
                    for k in keys {
                        adb::key(&phone.transport, &k).await?;
                    }
                }
                Ok(json!({"device": dev.name}))
            }
            Req::PairStart => {
                let code: [u8; 8] = rand::random();
                let code_s = data_encoding::HEXLOWER.encode(&code);
                *self.pair.lock().unwrap() = Some((code_s, Instant::now()));
                let mut raw = self.ep.id().as_bytes().to_vec();
                raw.extend_from_slice(&code);
                Ok(json!({"ticket": data_encoding::BASE32_NOPAD.encode(&raw).to_lowercase(), "minutes": PAIR_WINDOW.as_secs() / 60}))
            }
            Req::PairJoin { ticket } => {
                let raw = data_encoding::BASE32_NOPAD
                    .decode(ticket.trim().to_uppercase().as_bytes())
                    .ok()
                    .filter(|r| r.len() == 40)
                    .context("that is not a drop pairing ticket")?;
                let id = EndpointId::from_bytes(raw[..32].try_into()?)?;
                if id == self.ep.id() {
                    bail!("that ticket is from this device");
                }
                let code = data_encoding::HEXLOWER.encode(&raw[32..]);
                let me = self.cfg.lock().unwrap().name.clone();
                let reply = self.request(id, &Msg::Pair { code, name: me }).await?;
                let Msg::Ok { name: Some(name), .. } = reply else { bail!("unexpected reply") };
                let ids = id.to_string();
                self.update_cfg(|cfg| {
                    match cfg.devices.iter_mut().find(|d| d.id.as_deref() == Some(&ids)) {
                        Some(d) => d.name = name.clone(),
                        None => cfg.devices.push(Device { name: name.clone(), id: Some(ids.clone()), serial: None, clip: true }),
                    }
                    Ok(())
                })?;
                self.log("paired", &name, "");
                Ok(json!({"device": name}))
            }
            Req::Rename { device, name } => {
                if name == "." {
                    bail!("bad name");
                }
                let old = self.update_cfg(|c| {
                    if device == "." {
                        return Ok(std::mem::replace(&mut c.name, name.clone()));
                    }
                    let i = c.find(&device)?;
                    Ok(std::mem::replace(&mut c.devices[i].name, name.clone()))
                })?;
                Ok(json!({"old": old, "name": name}))
            }
            Req::Forget { device } => {
                let d = self.update_cfg(|c| {
                    let i = c.find(&device)?;
                    Ok(c.devices.remove(i))
                })?;
                if let Some(id) = d.id.as_deref().and_then(|i| i.parse::<EndpointId>().ok()) {
                    if let Some(c) = self.conns.lock().await.remove(&id) {
                        c.close(0u32.into(), b"forgotten");
                    }
                }
                Ok(json!({"forgot": d.name}))
            }
            Req::Localsend { mode } => {
                if let Some(m) = mode {
                    self.update_cfg(|c| {
                        c.localsend = Some(m);
                        Ok(())
                    })?;
                    self.start_localsend().await;
                    if let Some(ls) = self.ls() {
                        ls.nudge();
                    }
                }
                let ls = self.ls();
                Ok(json!({
                    "mode": self.ls_mode(),
                    "running": ls.is_some(),
                    "error": if self.ls_mode() == Mode::Off { None } else { self.ls_error.lock().unwrap().clone() },
                    "port": localsend::PORT,
                    "fingerprint": ls.as_ref().map(|l| l.fingerprint().to_string()),
                    "peers": ls.map(|l| l.peers()).unwrap_or_default(),
                }))
            }
            Req::Quickshare { mode } => {
                if let Some(m) = mode {
                    self.update_cfg(|c| {
                        c.quickshare = Some(m);
                        Ok(())
                    })?;
                    if m == Mode::Off {
                        self.qs.lock().unwrap().take();
                    } else {
                        self.start_quickshare().await;
                    }
                }
                let qs = self.qs.lock().unwrap().clone();
                Ok(json!({
                    "mode": self.qs_mode(),
                    "running": qs.is_some(),
                    "error": if self.qs_mode() == Mode::Off { None } else { self.qs_error.lock().unwrap().clone() },
                    "port": qs.as_ref().map(|q| q.port),
                    "name": qs.map(|q| q.name.clone()),
                }))
            }
            Req::Stop => Ok(json!({})),
        }
    }

    async fn send_one(&self, dev: &Device, phone: Option<&adb::Phone>, path: &Path, size: u64) -> Result<(String, String)> {
        if let Some(p) = phone.filter(|p| p.usb) {
            return Ok(("usb".into(), adb::push(&p.transport, path).await?));
        }
        if let Some(id) = dev.id.as_deref() {
            let id: EndpointId = id.parse()?;
            match self.send_net(id, path, size).await {
                Ok(r) => return Ok(r),
                Err(e) if phone.is_some() => eprintln!("net send failed, trying adb: {e:#}"),
                Err(e) => return Err(e),
            }
        }
        if let Some(p) = phone {
            return Ok((format!("adb {}", p.transport), adb::push(&p.transport, path).await?));
        }
        bail!("{} is not reachable right now (no drop app and no adb connection)", dev.name)
    }

    async fn send_net(&self, id: EndpointId, path: &Path, size: u64) -> Result<(String, String)> {
        let conn = self.conn(id).await?;
        let (mut s, mut r) = conn.open_bi().await?;
        let name = path.file_name().context("no file name")?.to_string_lossy().into_owned();
        proto::write(&mut s, &Msg::File { name, size }).await?;
        if let Msg::Err { error } = proto::read::<_, Msg>(&mut r).await? {
            bail!(error);
        }
        let file = tokio::fs::File::open(path).await?;
        let mut reader = tokio::io::BufReader::with_capacity(1 << 20, file);
        tokio::io::copy_buf(&mut reader, &mut s).await?;
        s.finish()?;
        match proto::read::<_, Msg>(&mut r).await? {
            Msg::Ok { detail, .. } => Ok((route(&conn), detail.unwrap_or_default())),
            Msg::Err { error } => bail!(error),
            other => bail!("unexpected {other:?}"),
        }
    }
}

fn merge_row(list: &mut Vec<Value>, name: &str, via: String, key: &str, val: Value, kind: Option<&str>) {
    if let Some(row) = list.iter_mut().find(|r| r["name"].as_str().is_some_and(|n| n.eq_ignore_ascii_case(name))) {
        if let Some(v) = row["via"].as_array_mut() {
            v.push(via.into());
        }
        row[key] = val;
        if row["type"].is_null() {
            row["type"] = json!(kind);
        }
        return;
    }
    let mut row = json!({"name": name, "id": null, "serial": null, "clip": false, "helper": false, "via": [via], "type": kind});
    row[key] = val;
    list.push(row);
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
enum Note<'a> {
    Plain,
    File(&'a Path),
    Image(&'a Path),
}

fn notify(title: &str, body: &str, note: Note) {
    #[cfg(target_os = "macos")]
    {
        let enc = |s: &str| {
            s.bytes()
                .map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
                .collect::<String>()
        };
        let key = store::menu_key().unwrap_or_default();
        let mut url = format!("dropmenu://notify?key={key}&title={}&body={}", enc(title), enc(body));
        match note {
            Note::Plain => {}
            Note::File(p) => url += &format!("&file={}", enc(&p.to_string_lossy())),
            Note::Image(p) => url += &format!("&image={}", enc(&p.to_string_lossy())),
        }
        let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
        let script = format!("display notification \"{}\" with title \"{}\"", esc(body), esc(title));
        std::thread::spawn(move || {
            let quiet = |c: &mut std::process::Command| {
                c.stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status().is_ok_and(|s| s.success())
            };
            if !quiet(std::process::Command::new("open").args(["-g", &url])) {
                quiet(std::process::Command::new("osascript").args(["-e", &script]));
            }
        });
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (title, body, note);
}
