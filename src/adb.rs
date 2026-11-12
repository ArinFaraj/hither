use anyhow::{Context, Result, bail};
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use tokio::process::Command;

#[derive(Clone, Debug, Serialize)]
pub struct Phone {
    pub transport: String,
    pub serial: String,
    pub name: String,
    pub usb: bool,
}

pub fn adb_path() -> Option<PathBuf> {
    static PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
    PATH.get_or_init(|| {
        let exe = "adb";
        let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
            .map(|p| std::env::split_paths(&p).collect())
            .unwrap_or_default();
        for var in ["ANDROID_HOME", "ANDROID_SDK_ROOT"] {
            if let Some(v) = std::env::var_os(var) {
                dirs.push(PathBuf::from(v).join("platform-tools"));
            }
        }
        let home = crate::store::home();
        dirs.push(home.join("Library/Android/sdk/platform-tools"));
        dirs.push(home.join("Android/Sdk/platform-tools"));
        dirs.push(home.join("scoop/shims"));
        dirs.extend(["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"].map(PathBuf::from));
        dirs.into_iter().map(|d| d.join(exe)).find(|p| p.is_file())
    })
    .clone()
}

fn adb() -> Result<Command> {
    let path = adb_path().context("adb not found")?;
    let mut cmd = Command::new(path);
    cmd.kill_on_drop(true);
    Ok(cmd)
}

async fn run(args: &[&str]) -> Result<String> {
    let out = adb()?.args(args).output().await?;
    if !out.status.success() {
        bail!("adb {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn cache() -> &'static Mutex<HashMap<String, (String, String)>> {
    static C: OnceLock<Mutex<HashMap<String, (String, String)>>> = OnceLock::new();
    C.get_or_init(Default::default)
}

pub async fn phones() -> Vec<Phone> {
    if adb_path().is_none() {
        return Vec::new();
    }
    let Ok(list) = run(&["devices", "-l"]).await else { return Vec::new() };
    let mut out = Vec::new();
    for line in list.lines().skip(1) {
        let mut parts = line.split_whitespace();
        let (Some(transport), Some("device")) = (parts.next(), parts.next()) else { continue };
        let rest: Vec<&str> = parts.collect();
        let usb = rest.iter().any(|p| p.starts_with("usb:")) || !transport.contains(':');
        let cached = cache().lock().unwrap().get(transport).cloned();
        let (serial, name) = match cached {
            Some(v) => v,
            None => {
                let props = shell(transport, "getprop ro.serialno; settings get global device_name; getprop ro.product.model")
                    .await
                    .unwrap_or_default();
                let mut l = props.lines().map(str::trim);
                let serial = l.next().filter(|s| !s.is_empty()).unwrap_or(transport).to_string();
                let name = l
                    .next()
                    .filter(|s| !s.is_empty() && *s != "null")
                    .or_else(|| l.next())
                    .unwrap_or(transport)
                    .to_string();
                cache().lock().unwrap().insert(transport.to_string(), (serial.clone(), name.clone()));
                (serial, name)
            }
        };
        out.push(Phone { transport: transport.to_string(), serial, name, usb });
    }
    out.sort_by_key(|p| !p.usb);
    out
}

pub async fn shell(transport: &str, script: &str) -> Result<String> {
    run(&["-s", transport, "shell", script]).await
}

pub async fn push(transport: &str, local: &Path) -> Result<String> {
    let name = local.file_name().context("not a file")?.to_string_lossy().into_owned();
    let remote = format!("/sdcard/Download/{name}");
    run(&["-s", transport, "push", &local.to_string_lossy(), &remote]).await?;
    let _ = shell(
        transport,
        &format!("cmd media.scan {0} >/dev/null 2>&1 || am broadcast -a android.intent.action.MEDIA_SCANNER_SCAN_FILE -d file://{0} >/dev/null 2>&1", quote(&remote)),
    )
    .await;
    Ok(remote)
}

pub async fn pull(transport: &str, remote: &str, dest: &Path) -> Result<()> {
    run(&["-s", transport, "pull", remote, &dest.to_string_lossy()]).await?;
    Ok(())
}

pub async fn read_uri(transport: &str, uri: &str, limit: u64) -> Result<Vec<u8>> {
    let out = adb()?.args(["-s", transport, "exec-out", "content", "read", "--uri", uri]).output().await?;
    if !out.status.success() || out.stdout.is_empty() {
        bail!("reading {uri}: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    if out.stdout.len() as u64 > limit {
        bail!("image is too large");
    }
    Ok(out.stdout)
}

pub async fn write_uri(transport: &str, uri: &str, data: &[u8]) -> Result<()> {
    use tokio::io::AsyncWriteExt;
    let mut child = adb()?
        .args(["-s", transport, "exec-in", "content", "write", "--uri", uri])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    let mut stdin = child.stdin.take().context("no stdin")?;
    stdin.write_all(data).await?;
    drop(stdin);
    let out = child.wait_with_output().await?;
    let said = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    if !out.status.success() || said.contains("Exception") || said.contains("Could not find provider") {
        if said.contains("provider") {
            bail!("the drop app is not installed on the phone (run: drop android install)");
        }
        bail!("writing {uri}: {}", said.trim());
    }
    Ok(())
}

pub async fn install(transport: &str, apk: &Path) -> Result<()> {
    run(&["-s", transport, "install", "-r", "-g", &apk.to_string_lossy()]).await?;
    let _ = run(&["-s", transport, "shell", "content", "query", "--uri", "content://drop.android.clip/start"]).await;
    Ok(())
}

pub fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

pub async fn type_text(transport: &str, text: &str) -> Result<()> {
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            key(transport, "enter").await?;
        }
        if line.is_empty() {
            continue;
        }
        if !line.is_ascii() {
            bail!("adb can only type plain ASCII and the drop helper is not running on the phone");
        }
        let escaped: String = line
            .chars()
            .map(|c| match c {
                ' ' => "%s".to_string(),
                c if c.is_ascii_alphanumeric() => c.to_string(),
                c => format!("\\{c}"),
            })
            .collect();
        shell(transport, &format!("input text {escaped}")).await?;
    }
    Ok(())
}

pub async fn key(transport: &str, name: &str) -> Result<()> {
    let code = match name.to_lowercase().as_str() {
        "enter" | "return" => "KEYCODE_ENTER".to_string(),
        "back" | "esc" | "escape" => "KEYCODE_BACK".to_string(),
        "home" => "KEYCODE_HOME".to_string(),
        "tab" => "KEYCODE_TAB".to_string(),
        "del" | "backspace" => "KEYCODE_DEL".to_string(),
        "up" | "down" | "left" | "right" => format!("KEYCODE_DPAD_{}", name.to_uppercase()),
        "recents" | "apps" => "KEYCODE_APP_SWITCH".to_string(),
        "power" => "KEYCODE_POWER".to_string(),
        "volup" => "KEYCODE_VOLUME_UP".to_string(),
        "voldown" => "KEYCODE_VOLUME_DOWN".to_string(),
        "paste" => "KEYCODE_PASTE".to_string(),
        other if other.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') => {
            format!("KEYCODE_{}", other.to_uppercase().trim_start_matches("KEYCODE_"))
        }
        other => bail!("unknown key {other}"),
    };
    shell(transport, &format!("input keyevent {code}")).await?;
    Ok(())
}
