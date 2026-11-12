use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub fn data_dir() -> PathBuf {
    if let Ok(d) = std::env::var("DROP_HOME") {
        return PathBuf::from(d);
    }
    let home = home();
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support/drop")
    } else {
        std::env::var("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| home.join(".local/share"))
            .join("drop")
    }
}

pub fn home() -> PathBuf {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

pub fn sock_path() -> PathBuf {
    data_dir().join("drop.sock")
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Device {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serial: Option<String>,
    #[serde(default = "yes")]
    pub clip: bool,
}

fn yes() -> bool {
    true
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Config {
    pub name: String,
    #[serde(default = "yes")]
    pub clip: bool,
    pub inbox: PathBuf,
    #[serde(default)]
    pub devices: Vec<Device>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub localsend: Option<crate::localsend::Mode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quickshare: Option<crate::localsend::Mode>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub localsend_trusted: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screenshots: Option<bool>,
}

impl Config {
    pub fn load_or_init() -> Result<Self> {
        let path = data_dir().join("config.json");
        if let Ok(bytes) = std::fs::read(&path) {
            return serde_json::from_slice(&bytes).with_context(|| format!("reading {}", path.display()));
        }
        let cfg = Config {
            name: host_name(),
            clip: true,
            inbox: default_inbox(),
            devices: Vec::new(),
            localsend: None,
            quickshare: None,
            localsend_trusted: Vec::new(),
            screenshots: None,
        };
        cfg.save()?;
        Ok(cfg)
    }

    pub fn save(&self) -> Result<()> {
        let path = data_dir().join("config.json");
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    pub fn find(&self, query: &str) -> Result<usize> {
        let q = query.to_lowercase();
        let by = |f: &dyn Fn(&Device) -> bool| -> Vec<usize> {
            self.devices.iter().enumerate().filter(|(_, d)| f(d)).map(|(i, _)| i).collect()
        };
        for hits in [
            by(&|d| d.name.to_lowercase() == q),
            by(&|d| d.serial.as_deref() == Some(query) || d.id.as_deref() == Some(query)),
            by(&|d| d.name.to_lowercase().starts_with(&q)),
            by(&|d| d.name.to_lowercase().contains(&q)),
            by(&|d| d.id.as_deref().is_some_and(|id| id.starts_with(&q))),
        ] {
            match hits.len() {
                0 => continue,
                1 => return Ok(hits[0]),
                _ => anyhow::bail!("\"{query}\" matches more than one device"),
            }
        }
        anyhow::bail!("no device called \"{query}\" (see `drop devices`)")
    }
}

fn default_inbox() -> PathBuf {
    let dl = home().join("Downloads");
    if dl.is_dir() { dl } else { data_dir().join("inbox") }
}

fn host_name() -> String {
    #[cfg(target_os = "macos")]
    if let Ok(out) = std::process::Command::new("scutil").args(["--get", "ComputerName"]).output() {
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !s.is_empty() {
            return s;
        }
    }
    if let Ok(s) = std::env::var("COMPUTERNAME") {
        return s;
    }
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .or_else(|| {
            std::process::Command::new("hostname")
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        })
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "device".into())
}

fn clean_name(name: &str) -> String {
    let clean: String = Path::new(name)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '/' | '\\' | ':'))
        .collect();
    if clean.is_empty() || clean.starts_with('.') { format!("file{clean}") } else { clean }
}

fn candidates(dir: &Path, name: &str) -> impl Iterator<Item = PathBuf> {
    let clean = clean_name(name);
    let p = Path::new(&clean);
    let stem = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = p.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    let dir = dir.to_path_buf();
    std::iter::once(dir.join(&clean)).chain((1..).map(move |n| dir.join(format!("{stem} ({n}){ext}"))))
}

fn part_path(dest: &Path) -> PathBuf {
    dest.with_file_name(format!(".{}.part", dest.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()))
}

pub fn unique_path(dir: &Path, name: &str) -> PathBuf {
    candidates(dir, name).find(|p| !p.exists()).expect("endless candidates")
}

pub fn reserve(dir: &Path, name: &str) -> std::io::Result<(PathBuf, PathBuf, std::fs::File)> {
    for dest in candidates(dir, name) {
        if dest.exists() {
            continue;
        }
        let part = part_path(&dest);
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&part) {
            Ok(f) => return Ok((dest, part, f)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    unreachable!()
}

pub fn menu_key() -> Result<String> {
    use std::io::Write;
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    let dir = data_dir();
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir)?;
    let path = dir.join("menu-key");
    if let Ok(k) = std::fs::read_to_string(&path) {
        if !k.trim().is_empty() {
            return Ok(k.trim().to_string());
        }
    }
    let key = data_encoding::HEXLOWER.encode(&rand::random::<[u8; 16]>());
    let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&path)?;
    f.write_all(key.as_bytes())?;
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserve_never_hands_out_the_same_name_twice() {
        let dir = std::env::temp_dir().join(format!("drop-reserve-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (a, _, _) = reserve(&dir, "photo.jpg").unwrap();
        let (b, _, _) = reserve(&dir, "photo.jpg").unwrap();
        let (c, _, _) = reserve(&dir, "../../photo.jpg").unwrap();
        assert_eq!(a, dir.join("photo.jpg"));
        assert_eq!(b, dir.join("photo (1).jpg"));
        assert_eq!(c, dir.join("photo (2).jpg"));
        let (d, _, _) = reserve(&dir, ".hidden").unwrap();
        assert_eq!(d, dir.join("file.hidden"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
