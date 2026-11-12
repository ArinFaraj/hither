use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::Command;

pub const REPO: &str = "ArinFaraj/hither";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
const RELEASE_KEY: &str = "295af6bc15ccd0d3d35a75dc3cdac06f286dba5928403e47e5b74bc82eaa17b9";

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Auto,
    #[default]
    Notify,
    Off,
}

#[derive(Deserialize, Clone, Debug)]
struct Asset {
    name: String,
    browser_download_url: String,
}

#[derive(Deserialize, Clone, Debug)]
pub struct Release {
    tag_name: String,
    #[serde(default)]
    html_url: String,
    assets: Vec<Asset>,
}

impl Release {
    pub fn version(&self) -> &str {
        self.tag_name.trim_start_matches('v')
    }

    pub fn url(&self) -> &str {
        &self.html_url
    }

    fn asset(&self, name: &str) -> Result<&Asset> {
        self.assets.iter().find(|a| a.name == name).with_context(|| format!("the release has no {name}"))
    }
}

fn parse(v: &str) -> Option<(u64, u64, u64)> {
    let core = v.trim_start_matches('v').split(['-', '+']).next()?;
    let mut it = core.split('.').map(|p| p.parse::<u64>().ok());
    let v = (it.next()??, it.next()??, it.next()??);
    it.next().is_none().then_some(v)
}

pub fn newer(latest: &str, current: &str) -> bool {
    matches!((parse(latest), parse(current)), (Some(a), Some(b)) if a > b)
}

pub fn android_version_code() -> u64 {
    let (a, b, c) = parse(VERSION).unwrap_or_default();
    a * 1_000_000 + b * 1_000 + c
}

fn platform() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Some("macos-arm64"),
        ("macos", "x86_64") => Some("macos-x86_64"),
        ("linux", "aarch64") => Some("linux-arm64"),
        ("linux", "x86_64") => Some("linux-x86_64"),
        _ => None,
    }
}

fn exe() -> Result<PathBuf> {
    Ok(std::fs::canonicalize(std::env::current_exe()?)?)
}

pub fn dev_build() -> bool {
    exe().map(|p| p.components().any(|c| c.as_os_str() == "target")).unwrap_or(true)
}

fn app_bundle(exe: &Path) -> Option<PathBuf> {
    exe.ancestors().find(|p| p.extension().is_some_and(|e| e == "app")).map(Path::to_path_buf)
}

fn curl(url: &str, out: Option<&Path>) -> Result<Vec<u8>> {
    let mut c = Command::new("curl");
    c.args(["-fsSL", "--proto", "=https", "--proto-redir", "=https", "--retry", "2", "--max-time", "900"]);
    c.args(["-H", "User-Agent: hither-updater", "-H", "Accept: application/vnd.github+json"]);
    if let Some(p) = out {
        c.arg("-o").arg(p);
    }
    let o = c.arg(url).output().context("updates need curl")?;
    ensure!(o.status.success(), "download of {url} failed: {}", String::from_utf8_lossy(&o.stderr).trim());
    Ok(o.stdout)
}

pub fn latest() -> Result<Release> {
    let body = curl(&format!("https://api.github.com/repos/{REPO}/releases/latest"), None)?;
    serde_json::from_slice(&body).context("GitHub sent an unexpected answer")
}

pub fn check() -> Result<Option<Release>> {
    let r = latest()?;
    Ok(newer(r.version(), VERSION).then_some(r))
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn verify_sums(sums: &[u8], sig: &[u8]) -> Result<()> {
    let key = data_encoding::HEXLOWER.decode(RELEASE_KEY.as_bytes())?;
    ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, key)
        .verify(sums, sig)
        .map_err(|_| anyhow::anyhow!("SHA256SUMS is not signed by the hither release key"))
}

fn expected_hash(sums: &str, name: &str) -> Result<String> {
    sums.lines()
        .filter_map(|l| l.split_once(char::is_whitespace))
        .find(|(_, n)| n.trim().trim_start_matches('*') == name)
        .map(|(h, _)| h.to_lowercase())
        .with_context(|| format!("SHA256SUMS does not list {name}"))
}

fn file_hash(path: &Path) -> Result<String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex(&h.finalize()))
}

fn run(cmd: &mut Command) -> Result<()> {
    let o = cmd.output().with_context(|| format!("running {:?}", cmd.get_program()))?;
    ensure!(o.status.success(), "{:?} failed: {}", cmd.get_program(), String::from_utf8_lossy(&o.stderr).trim());
    Ok(())
}

pub fn install(r: &Release, force: bool) -> Result<String> {
    let exe = exe()?;
    if dev_build() && !force {
        bail!("{} is a development build; update it from source, or pass --force", exe.display());
    }
    let plat = platform().context("there are no release builds for this system")?;
    let name = format!("hither-{}-{plat}.tar.gz", r.version());
    let sums = curl(&r.asset("SHA256SUMS")?.browser_download_url, None)?;
    let sig = curl(&r.asset("SHA256SUMS.sig")?.browser_download_url, None)?;
    verify_sums(&sums, &sig)?;
    let want = expected_hash(&String::from_utf8_lossy(&sums), &name)?;

    let work = crate::store::data_dir().join("update");
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work)?;
    let archive = work.join(&name);
    curl(&r.asset(&name)?.browser_download_url, Some(&archive))?;
    ensure!(file_hash(&archive)? == want, "{name} does not match its checksum");
    let out = work.join("x");
    std::fs::create_dir_all(&out)?;
    run(Command::new("tar").arg("-xzf").arg(&archive).arg("-C").arg(&out))?;

    let done = match app_bundle(&exe) {
        Some(app) if cfg!(target_os = "macos") => {
            let fresh = out.join("Hither.app");
            ensure!(fresh.join("Contents/MacOS/hither").is_file(), "the download has no Hither.app");
            let parent = app.parent().context("app has no folder")?;
            let staged = parent.join(".Hither.app.new");
            let old = parent.join(".Hither.app.old");
            let _ = std::fs::remove_dir_all(&staged);
            let _ = std::fs::remove_dir_all(&old);
            run(Command::new("ditto").arg(&fresh).arg(&staged))?;
            std::fs::rename(&app, &old)?;
            if let Err(e) = std::fs::rename(&staged, &app) {
                let _ = std::fs::rename(&old, &app);
                return Err(e.into());
            }
            let _ = std::fs::remove_dir_all(&old);
            app.display().to_string()
        }
        _ => {
            let fresh = if cfg!(target_os = "macos") { out.join("Hither.app/Contents/MacOS/hither") } else { out.join("hither") };
            ensure!(fresh.is_file(), "the download has no hither binary");
            let staged = exe.with_file_name(".hither.new");
            std::fs::copy(&fresh, &staged)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))?;
            }
            std::fs::rename(&staged, &exe)?;
            exe.display().to_string()
        }
    };
    let _ = std::fs::remove_dir_all(&work);
    Ok(done)
}

pub fn restart_menu() {
    #[cfg(target_os = "macos")]
    if let Some(app) = exe().ok().as_deref().and_then(app_bundle) {
        let running = Command::new("pgrep").args(["-x", "HitherMenu"]).output().is_ok_and(|o| o.status.success());
        if running {
            let _ = Command::new("pkill").args(["-x", "HitherMenu"]).status();
            std::thread::sleep(std::time::Duration::from_millis(500));
            let _ = Command::new("open").arg("-g").arg(&app).status();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_numerically() {
        assert!(newer("v0.10.0", "0.9.9"));
        assert!(newer("1.0.0", "0.99.99"));
        assert!(!newer("v0.1.0", "0.1.0"));
        assert!(!newer("0.1.0", "0.2.0"));
        assert!(!newer("garbage", "0.1.0"));
        assert!(!newer("1.2", "0.1.0"));
    }

    #[test]
    fn sums_lookup_matches_whole_names() {
        let sums = "aa  hither-1.0.0-linux-arm64.tar.gz\nbb *hither-1.0.0-linux-x86_64.tar.gz\n";
        assert_eq!(expected_hash(sums, "hither-1.0.0-linux-x86_64.tar.gz").unwrap(), "bb");
        assert!(expected_hash(sums, "hither-1.0.0-linux.tar.gz").is_err());
    }

    #[test]
    fn signatures_are_checked_against_the_release_key() {
        let sig = data_encoding::HEXLOWER
            .decode(b"66cd6baf9cc44c4be03c2508a375788fe5f9bc64c1214d12736a6defeb337af5195f313618ccb8f6e02f232df8bfb77a60ba23a7d09d908658bfa43a7411b108")
            .unwrap();
        assert!(verify_sums(b"hither release key test\n", &sig).is_ok());
        assert!(verify_sums(b"hither release key test!\n", &sig).is_err());
        assert!(verify_sums(b"hither release key test\n", &[0u8; 64]).is_err());
    }
}
