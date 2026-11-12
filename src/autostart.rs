use anyhow::{Result, bail};
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn exe() -> Result<PathBuf> {
    Ok(std::fs::canonicalize(std::env::current_exe()?)?)
}

fn quiet(cmd: &mut Command) -> bool {
    cmd.stdout(Stdio::null()).stderr(Stdio::null()).status().map(|s| s.success()).unwrap_or(false)
}

#[cfg(target_os = "macos")]
pub fn set(on: bool) -> Result<String> {
    let plist = crate::store::home().join("Library/LaunchAgents/hither.plist");
    let uid = String::from_utf8(Command::new("id").arg("-u").output()?.stdout)?.trim().to_string();
    let domain = format!("gui/{uid}");
    quiet(Command::new("launchctl").args(["bootout", &format!("{domain}/hither")]));
    if !on {
        let _ = std::fs::remove_file(&plist);
        return Ok("hither will no longer start at login".into());
    }
    let log = crate::store::data_dir().join("daemon.log");
    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>hither</string>
  <key>ProgramArguments</key><array><string>{}</string><string>daemon</string></array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
  <key>StandardOutPath</key><string>{}</string>
  <key>StandardErrorPath</key><string>{}</string>
</dict>
</plist>
"#,
        exe()?.display(),
        log.display(),
        log.display()
    );
    std::fs::create_dir_all(plist.parent().unwrap())?;
    std::fs::write(&plist, xml)?;
    if !quiet(Command::new("launchctl").args(["bootstrap", &domain, &plist.to_string_lossy()])) {
        bail!("launchctl could not load {}", plist.display());
    }
    Ok("hither starts at login and restarts if it crashes (LaunchAgent)".into())
}

#[cfg(all(unix, not(target_os = "macos")))]
pub fn set(on: bool) -> Result<String> {
    if quiet(Command::new("systemctl").args(["--user", "show-environment"])) {
        let unit = crate::store::home().join(".config/systemd/user/hither.service");
        if !on {
            quiet(Command::new("systemctl").args(["--user", "disable", "--now", "hither.service"]));
            let _ = std::fs::remove_file(&unit);
            return Ok("hither will no longer start on its own".into());
        }
        std::fs::create_dir_all(unit.parent().unwrap())?;
        std::fs::write(
            &unit,
            format!(
                "[Unit]\nDescription=hither\n\n[Service]\nExecStart={} daemon\nRestart=on-failure\n\n[Install]\nWantedBy=default.target\n",
                exe()?.display()
            ),
        )?;
        quiet(Command::new("systemctl").args(["--user", "daemon-reload"]));
        if !quiet(Command::new("systemctl").args(["--user", "enable", "--now", "hither.service"])) {
            bail!("systemctl --user enable hither.service failed");
        }
        return Ok("hither runs as a systemd user service".into());
    }
    use anyhow::Context;
    if on && !quiet(Command::new("pgrep").args(["-x", "cron|crond"])) {
        bail!("no systemd user session and no cron daemon here; start `hither daemon` from whatever supervises this machine");
    }
    let line = format!("*/2 * * * * {} status >/dev/null 2>&1", exe()?.display());
    let current = Command::new("crontab").arg("-l").stderr(Stdio::null()).output().context("crontab not found")?;
    let mut lines: Vec<String> = String::from_utf8_lossy(&current.stdout)
        .lines()
        .filter(|l| *l != line)
        .map(String::from)
        .collect();
    if on {
        lines.push(line);
    }
    let mut child = Command::new("crontab").arg("-").stdin(Stdio::piped()).spawn()?;
    use std::io::Write;
    let mut body = lines.join("\n");
    if !body.is_empty() {
        body.push('\n');
    }
    child.stdin.take().unwrap().write_all(body.as_bytes())?;
    if !child.wait()?.success() {
        bail!("crontab refused the update");
    }
    Ok(if on {
        "no systemd user session here, so cron checks every 2 minutes and starts hither if it is down".into()
    } else {
        "hither will no longer start on its own".into()
    })
}

#[cfg(not(unix))]
pub fn set(_on: bool) -> Result<String> {
    bail!("autostart is not supported on this system yet")
}
