use crate::adb;
use crate::clip::{self, Clip};
use anyhow::{Context, Result, anyhow, bail};
use data_encoding::BASE64;
use std::collections::VecDeque;
use std::process::Stdio;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{mpsc, oneshot};

const JAR: &[u8] = include_bytes!("../assets/drop-helper.jar");

pub struct Session {
    pub serial: String,
    pub transport: String,
    stdin: tokio::sync::Mutex<ChildStdin>,
    waiters: Mutex<VecDeque<oneshot::Sender<Result<String, String>>>>,
    alive: AtomicBool,
    child: Mutex<Option<Child>>,
}

pub struct ClipEvent {
    pub serial: String,
    pub clip: Clip,
}

pub const CLIP_URI: &str = "content://drop.android.clip/in";

fn remote_jar() -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    JAR.hash(&mut hasher);
    let h = hasher.finish();
    format!("/data/local/tmp/drop-helper-{:08x}.jar", h as u32)
}

impl Session {
    pub fn alive(&self) -> bool {
        self.alive.load(Ordering::Relaxed)
    }

    pub fn kill(&self) {
        self.alive.store(false, Ordering::Relaxed);
        if let Some(mut c) = self.child.lock().unwrap().take() {
            let _ = c.start_kill();
        }
        for w in self.waiters.lock().unwrap().drain(..) {
            let _ = w.send(Err("phone disconnected".into()));
        }
    }

    pub async fn start(serial: &str, transport: &str, events: mpsc::UnboundedSender<ClipEvent>) -> Result<std::sync::Arc<Self>> {
        let jar = remote_jar();
        let check = adb::shell(transport, &format!("[ -f {jar} ] && echo ok")).await.unwrap_or_default();
        if check.trim() != "ok" {
            let tmp = crate::store::data_dir().join("drop-helper.jar");
            tokio::fs::write(&tmp, JAR).await?;
            let out = Command::new(adb::adb_path().context("adb not found")?)
                .args(["-s", transport, "push", &tmp.to_string_lossy(), &jar])
                .output()
                .await?;
            if !out.status.success() {
                bail!("pushing helper: {}", String::from_utf8_lossy(&out.stderr).trim());
            }
            let _ = adb::shell(
                transport,
                &format!("for f in /data/local/tmp/drop-helper-*.jar; do [ \"$f\" = {jar} ] || rm -f \"$f\"; done"),
            )
            .await;
        }
        let mut child = Command::new(adb::adb_path().context("adb not found")?)
            .args(["-s", transport, "shell", &format!("CLASSPATH={jar} exec app_process / drop.Helper")])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        let stdin = child.stdin.take().context("no stdin")?;
        let mut lines = BufReader::new(child.stdout.take().context("no stdout")?).lines();
        let first = tokio::time::timeout(Duration::from_secs(10), lines.next_line())
            .await
            .map_err(|_| anyhow!("helper did not start"))??
            .unwrap_or_default();
        if !first.starts_with('R') {
            bail!("helper said {first:?}");
        }
        let s = std::sync::Arc::new(Session {
            serial: serial.to_string(),
            transport: transport.to_string(),
            stdin: tokio::sync::Mutex::new(stdin),
            waiters: Mutex::new(VecDeque::new()),
            alive: AtomicBool::new(true),
            child: Mutex::new(Some(child)),
        });
        let reader = s.clone();
        tokio::spawn(async move {
            while let Ok(Some(line)) = lines.next_line().await {
                let (op, arg) = line.split_at(line.len().min(1));
                let arg = arg.trim_start();
                let text = || BASE64.decode(arg.as_bytes()).ok().map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
                match op {
                    "C" => {
                        let _ = events.send(ClipEvent { serial: reader.serial.clone(), clip: Clip::Text(text()) });
                    }
                    "I" => {
                        let (uri, transport, serial, events) = (text(), reader.transport.clone(), reader.serial.clone(), events.clone());
                        tokio::spawn(async move {
                            match adb::read_uri(&transport, &uri, clip::MAX_IMAGE).await {
                                Ok(b) => {
                                    let _ = events.send(ClipEvent { serial, clip: Clip::Image(b) });
                                }
                                Err(e) => eprintln!("image clipboard on {serial}: {e:#}"),
                            }
                        });
                    }
                    "K" | "V" => {
                        if let Some(w) = reader.waiters.lock().unwrap().pop_front() {
                            let _ = w.send(Ok(text()));
                        }
                    }
                    "E" => {
                        let msg = text();
                        match reader.waiters.lock().unwrap().pop_front() {
                            Some(w) => {
                                let _ = w.send(Err(msg));
                            }
                            None => eprintln!("helper on {}: {msg}", reader.serial),
                        }
                    }
                    _ => {}
                }
            }
            reader.kill();
        });
        Ok(s)
    }

    async fn request(&self, op: char, arg: &str) -> Result<String> {
        if !self.alive() {
            bail!("phone helper is not running");
        }
        let (tx, rx) = oneshot::channel();
        {
            let mut stdin = self.stdin.lock().await;
            self.waiters.lock().unwrap().push_back(tx);
            let line = format!("{op} {}\n", BASE64.encode(arg.as_bytes()));
            if let Err(e) = stdin.write_all(line.as_bytes()).await {
                self.kill();
                return Err(e.into());
            }
        }
        match tokio::time::timeout(Duration::from_secs(20), rx).await {
            Ok(Ok(Ok(v))) => Ok(v),
            Ok(Ok(Err(e))) => bail!("phone: {e}"),
            _ => {
                self.kill();
                bail!("phone helper stopped answering")
            }
        }
    }

    pub async fn set(&self, clip: &Clip) -> Result<()> {
        match clip {
            Clip::Text(t) => self.request('S', t).await.map(|_| ()),
            Clip::Image(b) => adb::write_uri(&self.transport, CLIP_URI, b).await,
        }
    }

    pub async fn get_clip(&self) -> Result<String> {
        self.request('G', "").await
    }

    pub async fn type_text(&self, text: &str) -> Result<()> {
        self.request('T', text).await.map(|_| ())
    }

    pub async fn keys(&self, codes: &[i32]) -> Result<()> {
        let s: Vec<String> = codes.iter().map(i32::to_string).collect();
        self.request('K', &s.join(" ")).await.map(|_| ())
    }
}

pub async fn track(changed: mpsc::UnboundedSender<()>) {
    loop {
        let Some(path) = adb::adb_path() else {
            tokio::time::sleep(Duration::from_secs(300)).await;
            continue;
        };
        let child = Command::new(path)
            .arg("track-devices")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn();
        if let Ok(mut child) = child {
            if let Some(mut out) = child.stdout.take() {
                let mut len = [0u8; 4];
                while out.read_exact(&mut len).await.is_ok() {
                    let n = usize::from_str_radix(std::str::from_utf8(&len).unwrap_or("0"), 16).unwrap_or(0);
                    let mut body = vec![0u8; n];
                    if out.read_exact(&mut body).await.is_err() {
                        break;
                    }
                    let _ = changed.send(());
                }
            }
            let _ = child.kill().await;
        }
        let _ = changed.send(());
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

pub fn keycode(name: &str) -> Result<i32> {
    Ok(match name.to_lowercase().as_str() {
        "enter" | "return" => 66,
        "back" | "esc" | "escape" => 4,
        "home" => 3,
        "tab" => 61,
        "del" | "backspace" => 67,
        "delete" | "forward_del" => 112,
        "up" => 19,
        "down" => 20,
        "left" => 21,
        "right" => 22,
        "space" => 62,
        "recents" | "apps" => 187,
        "power" => 26,
        "volup" => 24,
        "voldown" => 25,
        "mute" => 164,
        "play" | "pause" | "playpause" => 85,
        "next" => 87,
        "prev" | "previous" => 88,
        "paste" => 279,
        "copy" => 278,
        "cut" => 277,
        n if n.parse::<i32>().is_ok() => n.parse()?,
        other => bail!("unknown key {other}"),
    })
}
