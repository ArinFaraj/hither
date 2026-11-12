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
    } else if cfg!(windows) {
        std::env::var("APPDATA").map(PathBuf::from).unwrap_or(home).join("drop")
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub localsend_trusted: Vec<String>,
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
            localsend_trusted: Vec::new(),
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

pub fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let clean: String = Path::new(name)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '/' | '\\' | ':'))
        .collect();
    let clean = if clean.is_empty() || clean.starts_with('.') { format!("file{clean}") } else { clean };
    let first = dir.join(&clean);
    if !first.exists() {
        return first;
    }
    let p = Path::new(&clean);
    let stem = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = p.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    (1..)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|p| !p.exists())
        .unwrap()
}
