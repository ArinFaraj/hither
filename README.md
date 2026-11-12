# drop

Send files, share the clipboard and type on your own devices, and show up for
everyone else's: LocalSend, Quick Share and AirDrop users can see you and send
to you without installing anything.

drop is one small Rust binary. It runs as a background service and picks the
best way to reach each device by itself: USB, the local network, Tailscale, or
the internet through a relay when nothing closer works.

## What it does

- **Files** to and from your devices, any size, with `drop send <device> <file>`.
- **Clipboard sync**: text and images. Copy on one device, paste on the others.
- **Screenshots to the clipboard**: take a screenshot on the Mac or on an
  Android phone with the app and it is ready to paste on your other devices.
  Turn it off with `drop screenshots off` or in the Android app.
- **Phones over adb**: send files, read and set the clipboard, and type text
  on an Android phone over USB or wireless debugging. No app needed for this.
- **LocalSend**: drop shows up as a LocalSend device and can send to LocalSend
  apps on any platform.
- **Quick Share**: Android phones nearby can send to drop with the share sheet,
  and drop can send to them.
- **AirDrop** (macOS): send files or the clipboard through the system AirDrop
  picker. Files that arrive by AirDrop show up in the drop menu.
- **Mac menu bar app** with every device in one list, drag and drop, a
  button to send the clipboard, and notifications for whatever arrives.
  Click a file notification to show the file in Finder.
- **Android app** with a share sheet entry and a Quick Settings tile that sends
  the clipboard to the device you picked.

A device that is reachable several ways (for example your phone over USB and
over Quick Share) is shown once, and drop uses whichever way works.

## Install

drop runs on macOS and Linux. You need Rust 1.85 or newer.

```sh
cargo build --release
./target/release/drop autostart on
```

`drop autostart on` installs a launchd agent on macOS or a systemd user unit
on Linux so the service starts at login.

On macOS, build the menu bar app with:

```sh
./scripts/build-mac-app.sh
cp -R target/Drop.app ~/Applications/
```

### Android

Connect the phone with USB debugging on, then:

```sh
drop android install
```

The app needs Android 13 or newer. `drop android install` grants it access to
your photos so new screenshots reach your other devices. To build the APK yourself you need the
Android SDK (no Gradle): `./android/build.sh` writes `assets/drop.apk`, which
gets embedded in the drop binary.

## Use

```sh
drop pair                  # on the first device, prints a ticket
drop pair <ticket>         # on the second device
drop devices               # what drop can reach, and how
drop send phone photo.jpg  # device names match by prefix
drop clip on               # sync the clipboard with all paired devices
drop airdrop               # AirDrop the clipboard (macOS)
```

Run `drop help` for every command. Add `--json` to any command for machine
readable output.

LocalSend and Quick Share both ask before accepting anything from a device you
have not trusted. Change that with `drop localsend ask|always|off` and
`drop quickshare ask|always|off`.

## How it works

- Paired devices talk over [iroh](https://iroh.computer) QUIC connections,
  authenticated by their keys. Pairing uses a one time ticket.
- Phones are reached through adb. drop starts a small helper on the phone with
  `app_process` to watch the clipboard, so nothing needs to be installed for
  that part.
- LocalSend is the open v2 protocol over HTTPS with a self signed certificate.
- Quick Share is a clean room implementation written from the protocol notes in
  [NearDrop](https://github.com/grishka/NearDrop) (Unlicense).
- AirDrop is not reimplemented. On macOS drop uses the system sharing service.

## Building for Linux arm64 from a Mac

`./scripts/build-linux-arm64.sh` cross compiles a static musl binary using the
clang from the Android NDK.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.
