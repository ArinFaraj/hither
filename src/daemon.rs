use crate::proto::{self, ALPN, Msg};
use crate::store::{self, Config, Device};
use crate::phone::{self, ClipEvent, Session};
use crate::{adb, clip};
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
    ClipSync { on: bool, device: Option<String> },
    Type { to: String, text: String },
    Key { to: String, keys: Vec<String> },
    PairStart,
    PairJoin { ticket: String },
    Rename { device: String, name: String },
    Forget { device: String },
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
    me: std::sync::Weak<State>,
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
        last_clip: Mutex::new(clip::get().map(|t| clip::hash(&t)).unwrap_or(0)),
        recent: Mutex::new(VecDeque::new()),
        adopt: adopt_tx,
        phones: Mutex::new(HashMap::new()),
        clip_events: clip_tx,
        me: me.clone(),
    });
    {
        let st = st.clone();
        tokio::spawn(async move {
            while let Some(ev) = clip_rx.recv().await {
                st.spread(Source::Phone(ev.serial), ev.text).await;
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
        let Some(text) = clip::get() else { continue };
        st.spread(Source::Local, text).await;
    }
}

impl State {
    async fn spread(&self, src: Source, text: String) {
        if text.is_empty() || !self.cfg.lock().unwrap().clip {
            return;
        }
        let h = clip::hash(&text);
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
            let t = text.clone();
            let _ = tokio::task::spawn_blocking(move || clip::set(&t)).await;
        }
        if src != Source::Local {
            self.log("clip_in", &from, format!("{} chars", text.chars().count()));
        }
        let Some(me) = self.me.upgrade() else { return };
        let to_net = !matches!(src, Source::Net(_));
        for d in devices.into_iter().filter(|d| d.clip) {
            if let (true, Some(id)) = (to_net, d.id.as_deref().and_then(|i| i.parse::<EndpointId>().ok())) {
                if src != Source::Net(id) {
                    let (me, text, d) = (me.clone(), text.clone(), d.clone());
                    tokio::spawn(async move {
                        if let Err(e) = me.clip_to(&d, &text).await {
                            eprintln!("clipboard to {}: {e:#}", d.name);
                        }
                    });
                }
            }
            if let Some(serial) = d.serial.as_ref().filter(|s| src != Source::Phone((*s).clone())) {
                if let Some(sess) = self.session(serial) {
                    let text = text.clone();
                    tokio::spawn(async move {
                        if let Err(e) = sess.set_clip(&text).await {
                            eprintln!("clipboard to phone {}: {e:#}", sess.serial);
                        }
                    });
                }
            }
        }
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

    async fn clip_to(&self, d: &Device, text: &str) -> Result<()> {
        let id: EndpointId = d.id.as_deref().context("no network id")?.parse()?;
        self.request(id, &Msg::Clip { text: text.to_string() }).await?;
        Ok(())
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
                    self.spread(Source::Net(peer), text).await;
                }
                Ok(Msg::Ok { name: None, detail: None })
            }
            (Msg::File { name, size }, Some(dev)) => {
                proto::write(&mut send, &Msg::Ok { name: None, detail: None }).await?;
                let started = Instant::now();
                match self.receive_file(&mut recv, &name, size).await {
                    Ok(path) => {
                        let secs = started.elapsed().as_secs_f64();
                        self.log("file_in", &dev.name, path.display().to_string());
                        notify(&format!("From {}", dev.name), &name);
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
        let dest = store::unique_path(&inbox, name);
        let part = dest.with_file_name(format!(
            ".{}.part",
            dest.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
        ));
        let file = tokio::fs::File::create(&part).await?;
        let mut out = tokio::io::BufWriter::with_capacity(1 << 20, file);
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
        notify("Paired", name);
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
                Ok(json!({"devices": list}))
            }
            Req::Recent => Ok(json!({"recent": self.recent.lock().unwrap().iter().cloned().collect::<Vec<_>>()})),
            Req::Send { to, paths } => {
                let (dev, phone) = self.resolve(&to).await?;
                let mut results = Vec::new();
                for path in paths {
                    let started = Instant::now();
                    let size = tokio::fs::metadata(&path).await.with_context(|| path.display().to_string())?.len();
                    if !path.is_file() {
                        bail!("{} is not a file", path.display());
                    }
                    let (via, dest) = self.send_one(&dev, phone.as_ref(), &path, size).await?;
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
            Req::ClipGet { from: None } => Ok(json!({"text": clip::get()})),
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
                tokio::task::spawn_blocking(move || clip::set(&text)).await?;
                Ok(json!({}))
            }
            Req::ClipSet { text, to: Some(to) } => {
                let (dev, _) = self.resolve(&to).await?;
                if let Some(sess) = dev.serial.as_deref().and_then(|s| self.session(s)) {
                    sess.set_clip(&text).await?;
                } else if dev.id.is_some() {
                    self.clip_to(&dev, &text).await?;
                } else {
                    bail!("{}'s clipboard is not reachable right now", dev.name);
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

fn notify(title: &str, body: &str) {
    #[cfg(target_os = "macos")]
    {
        let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
        let script = format!("display notification \"{}\" with title \"{}\"", esc(body), esc(title));
        let _ = std::process::Command::new("osascript")
            .args(["-e", &script])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (title, body);
}
