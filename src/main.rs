mod adb;
mod autostart;
mod clip;
mod daemon;
mod localsend;
mod phone;
mod proto;
mod store;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

const USAGE: &str = "drop: send files, share the clipboard and type on your devices

usage:
  drop devices                     list devices and how each one is reached
  drop send <device> <file>...     send files (USB, LAN, Tailscale or internet, picked for you)
  drop get <device> <path> [dest]  copy a file off a phone
  drop clip [get <device>]         print the clipboard (here, or on a phone)
  drop clip set <text|->           set the clipboard here (it syncs to your devices)
  drop clip send <device> <text|-> put text on one device's clipboard
  drop clip on|off [device]        turn clipboard sync on or off
  drop type <device> <text|->      type text on a phone
  drop key <device> <key>...       press keys (enter, back, home, tab, del, up, ...)
  drop pair                        show a ticket for another device to join
  drop pair <ticket>               join the device that showed the ticket
  drop rename <device|.> <name>    rename a device (. is this one)
  drop forget <device>             remove a device
  drop autostart on|off            keep drop running in the background
  drop localsend [ask|always|off]  let LocalSend apps nearby see this device and send to it
  drop status | recent | stop
  drop daemon                      run the background service

add --json to any command for machine output.";

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let json_out = take_flag(&mut args, "--json");
    if args.first().map(String::as_str) == Some("daemon") {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        if let Err(e) = rt.block_on(daemon::run()) {
            eprintln!("drop: {e:#}");
            std::process::exit(1);
        }
        return;
    }
    if let ["autostart", v @ ("on" | "off")] = args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        if let Ok(mut s) = UnixStream::connect(store::sock_path()) {
            let _ = s.write_all(b"{\"cmd\":\"stop\"}\n");
            let _ = s.read_to_end(&mut Vec::new());
            std::thread::sleep(Duration::from_millis(300));
        }
        match autostart::set(*v == "on") {
            Ok(msg) => println!("{msg}"),
            Err(e) => {
                eprintln!("drop: {e:#}");
                std::process::exit(1);
            }
        }
        return;
    }
    match run(&args) {
        Ok(Some(req)) => match call(&req) {
            Ok(reply) => {
                let ok = reply["ok"].as_bool().unwrap_or(false);
                if json_out {
                    println!("{reply}");
                } else if ok {
                    print_human(&req, &reply);
                } else {
                    eprintln!("drop: {}", reply["error"].as_str().unwrap_or("failed"));
                }
                std::process::exit(if ok { 0 } else { 1 });
            }
            Err(e) => {
                if json_out {
                    println!("{}", json!({"ok": false, "error": format!("{e:#}")}));
                } else {
                    eprintln!("drop: {e:#}");
                }
                std::process::exit(3);
            }
        },
        Ok(None) => println!("{USAGE}"),
        Err(e) => {
            eprintln!("drop: {e:#}\n\n{USAGE}");
            std::process::exit(2);
        }
    }
}

fn take_flag(args: &mut Vec<String>, flag: &str) -> bool {
    let before = args.len();
    args.retain(|a| a != flag);
    args.len() != before
}

fn text_arg(rest: &[String]) -> Result<String> {
    if rest.is_empty() || rest == ["-"] {
        let mut s = String::new();
        std::io::stdin().read_to_string(&mut s)?;
        return Ok(s);
    }
    Ok(rest.join(" "))
}

fn run(args: &[String]) -> Result<Option<Value>> {
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    let req = match a.as_slice() {
        [] | ["help" | "-h" | "--help"] => return Ok(None),
        ["status"] => json!({"cmd": "status"}),
        ["devices" | "ls"] => json!({"cmd": "devices"}),
        ["recent"] => json!({"cmd": "recent"}),
        ["stop"] => json!({"cmd": "stop"}),
        ["send", to, files @ ..] if !files.is_empty() => {
            let paths = files
                .iter()
                .map(|f| std::fs::canonicalize(f).with_context(|| format!("{f}: no such file")))
                .collect::<Result<Vec<_>>>()?;
            json!({"cmd": "send", "to": to, "paths": paths})
        }
        ["get", from, path] => json!({"cmd": "get", "from": from, "path": path}),
        ["get", from, path, dest] => {
            let mut d = std::path::PathBuf::from(dest);
            if d.is_relative() {
                d = std::env::current_dir()?.join(d);
            }
            if d.is_dir() {
                d = d.join(path.rsplit('/').next().unwrap_or("file"));
            }
            json!({"cmd": "get", "from": from, "path": path, "dest": d})
        }
        ["clip"] | ["clip", "get"] => json!({"cmd": "clip_get"}),
        ["clip", "get", from] => json!({"cmd": "clip_get", "from": from}),
        ["clip", "set", ..] => json!({"cmd": "clip_set", "text": text_arg(&args[2..])?}),
        ["clip", "send", to, ..] => json!({"cmd": "clip_set", "to": to, "text": text_arg(&args[3..])?}),
        ["clip", on @ ("on" | "off")] => json!({"cmd": "clip_sync", "on": *on == "on"}),
        ["clip", on @ ("on" | "off"), dev] => json!({"cmd": "clip_sync", "on": *on == "on", "device": dev}),
        ["type", to, ..] => json!({"cmd": "type", "to": to, "text": text_arg(&args[2..])?}),
        ["key", to, keys @ ..] if !keys.is_empty() => json!({"cmd": "key", "to": to, "keys": keys}),
        ["pair"] => json!({"cmd": "pair_start"}),
        ["pair", ticket] => json!({"cmd": "pair_join", "ticket": ticket}),
        ["rename", dev, name @ ..] if !name.is_empty() => json!({"cmd": "rename", "device": dev, "name": name.join(" ")}),
        ["forget", dev] => json!({"cmd": "forget", "device": dev}),
        ["localsend"] => json!({"cmd": "localsend"}),
        ["localsend", m @ ("ask" | "always" | "off")] => json!({"cmd": "localsend", "mode": m}),
        _ => bail!("unknown command: {}", args.join(" ")),
    };
    Ok(Some(req))
}

fn connect() -> Result<UnixStream> {
    let sock = store::sock_path();
    if let Ok(s) = UnixStream::connect(&sock) {
        return Ok(s);
    }
    std::fs::create_dir_all(store::data_dir())?;
    let log = std::fs::OpenOptions::new().create(true).append(true).open(store::data_dir().join("daemon.log"))?;
    let mut cmd = std::process::Command::new(std::env::current_exe()?);
    cmd.arg("daemon").stdin(std::process::Stdio::null()).stdout(log.try_clone()?).stderr(log);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }
    cmd.spawn().context("starting the drop service")?;
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(8) {
        std::thread::sleep(Duration::from_millis(100));
        if let Ok(s) = UnixStream::connect(&sock) {
            return Ok(s);
        }
    }
    bail!("the drop service did not start; see {}", store::data_dir().join("daemon.log").display())
}

fn call(req: &Value) -> Result<Value> {
    let mut s = connect()?;
    let mut line = serde_json::to_vec(req)?;
    line.push(b'\n');
    s.write_all(&line)?;
    let mut reply = String::new();
    BufReader::new(s).read_line(&mut reply)?;
    if reply.is_empty() && req["cmd"] == "stop" {
        return Ok(json!({"ok": true}));
    }
    Ok(serde_json::from_str(&reply)?)
}

fn human_bytes(n: f64) -> String {
    let units = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n;
    let mut i = 0;
    while v >= 1000.0 && i < units.len() - 1 {
        v /= 1000.0;
        i += 1;
    }
    if i == 0 { format!("{v:.0} {}", units[i]) } else { format!("{v:.1} {}", units[i]) }
}

fn print_human(req: &Value, r: &Value) {
    let s = |v: &Value| v.as_str().unwrap_or("").to_string();
    match req["cmd"].as_str().unwrap_or("") {
        "status" => {
            println!("{}  ({})", s(&r["name"]), s(&r["id"]));
            println!("clipboard sync: {}", if r["clip"] == true { "on" } else { "off" });
            println!("inbox: {}", s(&r["inbox"]));
            println!("adb: {}", r["adb"].as_str().unwrap_or("not found"));
            println!("devices: {}", r["devices"]);
        }
        "devices" => {
            let list = r["devices"].as_array().cloned().unwrap_or_default();
            if list.is_empty() {
                println!("no devices yet. run `drop pair` here and `drop pair <ticket>` on the other one, or plug in a phone with adb on.");
            }
            for d in list {
                let via: Vec<String> = d["via"].as_array().into_iter().flatten().map(&s).collect();
                let via = if via.is_empty() { "offline".to_string() } else { via.join(", ") };
                let clip = if d["clip"] == true || d["localsend"].is_string() { "" } else { "  (clipboard off)" };
                println!("{:<24} {}{}", s(&d["name"]), via, clip);
            }
        }
        "recent" => {
            for e in r["recent"].as_array().into_iter().flatten() {
                println!("{:<9} {:<20} {}", s(&e["kind"]), s(&e["device"]), s(&e["detail"]));
            }
        }
        "send" => {
            for f in r["sent"].as_array().into_iter().flatten() {
                let bytes = f["bytes"].as_f64().unwrap_or(0.0);
                let secs = f["secs"].as_f64().unwrap_or(0.0).max(0.001);
                println!(
                    "sent {} to {} via {} ({}, {}/s)",
                    std::path::Path::new(&s(&f["file"])).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                    s(&r["device"]),
                    s(&f["via"]),
                    human_bytes(bytes),
                    human_bytes(bytes / secs)
                );
            }
        }
        "get" => println!("{}", s(&r["saved"])),
        "clip_get" => {
            if let Some(t) = r["text"].as_str() {
                print!("{t}");
                if !t.ends_with('\n') {
                    println!();
                }
            }
        }
        "clip_sync" => println!("clipboard sync {}", if r["clip"] == true { "on" } else { "off" }),
        "pair_start" => {
            println!("on the other device run:\n\n  drop pair {}\n", s(&r["ticket"]));
            println!("the ticket works once, for {} minutes.", r["minutes"]);
        }
        "pair_join" => println!("paired with {}", s(&r["device"])),
        "rename" => println!("{} is now {}", s(&r["old"]), s(&r["name"])),
        "forget" => println!("forgot {}", s(&r["forgot"])),
        "localsend" => {
            let mode = s(&r["mode"]);
            match (mode.as_str(), r["running"] == true) {
                ("off", _) => println!("localsend: off"),
                (_, true) => println!("localsend: {mode}, listening on port {}", r["port"]),
                (_, false) => println!("localsend: {mode}, not running ({})", s(&r["error"])),
            }
            for p in r["peers"].as_array().into_iter().flatten() {
                println!("  {:<24} {}", s(&p["alias"]), s(&p["ip"]));
            }
        }
        _ => {}
    }
}
