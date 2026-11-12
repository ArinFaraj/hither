<p align="center"><img src="assets/icon.png" width="128" alt=""></p>

<h1 align="center">hither</h1>

<p align="center">Everything comes here. Files, the clipboard and screenshots move between your own
devices by themselves, and people nearby can send to you from LocalSend, Quick Share
or AirDrop without installing anything.</p>

hither is one small Rust binary that runs in the background. It picks the best way
to reach each device on its own: USB, the local network, Tailscale, or the internet
through a relay when nothing closer works.

## What it does

- **Files** to and from your devices, any size: `hither send phone photo.jpg`.
- **Clipboard sync** for text and images. Copy on one device, paste on the others.
- **Screenshots to the clipboard.** Take one on the Mac and it is ready to paste
  on your other devices.
- **Phones over adb.** Send files, read and set the clipboard, and type on an
  Android phone over USB or wireless debugging, with no app needed.
- **LocalSend.** hither shows up as a LocalSend device and can send to LocalSend on
  any platform.
- **Quick Share.** Android phones nearby can send to hither from the share sheet,
  and hither can send to them.
- **AirDrop** (macOS). Send files or the clipboard through the system AirDrop
  picker. Files that arrive by AirDrop show up in the hither menu.
- **A Mac menu bar app** with every device in one list, drag and drop, and a
  button to send the clipboard.
- **An Android app** with a share sheet entry and a Quick Settings tile that sends
  the clipboard to the device you picked.
- **Updates itself** from GitHub releases, with signed checksums.

A device that is reachable several ways (your phone over USB and over Quick Share,
say) is listed once, and hither uses whichever way works.

## Install

macOS (Apple silicon or Intel) and Linux (x86_64 or arm64):

```sh
curl -fsSL https://raw.githubusercontent.com/ArinFaraj/hither/main/install.sh | sh
```

On a Mac this puts `Hither.app` in `~/Applications`, links `hither` into
`~/.local/bin` and starts the menu bar app. On Linux it installs
`~/.local/bin/hither`. Both set it up to start at login.

You can also download a build from the
[releases page](https://github.com/ArinFaraj/hither/releases) yourself. The Mac
app is not notarized, so if you download it with a browser macOS will refuse to
open it. Clear the quarantine flag once and it opens normally:

```sh
xattr -dr com.apple.quarantine ~/Applications/Hither.app
```

### Android

Turn on USB debugging, connect the phone, then:

```sh
hither android install
```

The app needs Android 13 or newer. After that hither keeps it up to date whenever
the phone is connected over adb.

## Use

```sh
hither pair                  # on the first device, prints a ticket
hither pair <ticket>         # on the second device
hither devices               # what hither can reach, and how
hither send phone photo.jpg  # device names match by prefix
hither clip on               # sync the clipboard with all paired devices
hither airdrop               # AirDrop the clipboard (macOS)
```

Run `hither help` for every command. Add `--json` to any command for machine
readable output.

LocalSend and Quick Share ask before accepting anything from a device you have not
trusted. Change that with `hither localsend ask|always|off` and
`hither quickshare ask|always|off`.

## Updates

hither checks GitHub for a new release once a day. By default it only tells you
(a notification, and an Update button in the Mac menu). To choose:

```sh
hither update --check        # is there a newer release?
hither update                # install it now
hither updates auto          # install new releases by itself
hither updates notify        # just tell me (the default)
hither updates off           # never check
```

Every release has a `SHA256SUMS` file signed with an Ed25519 key whose public half
is built into hither. An update is only installed if the signature and the checksum
both match. A build running out of a cargo `target` folder is never replaced.

## Security

- Paired devices talk over [iroh](https://iroh.computer) QUIC connections and are
  identified by their keys. Pairing uses a one time ticket that expires after 10
  minutes. A device that is not paired gets one request (the pairing attempt) and
  is disconnected.
- LocalSend runs over HTTPS with a self signed certificate. Outgoing connections are
  pinned to the fingerprint the other device announced. "Always accept" is only
  offered for senders that prove their fingerprint, either with a TLS client
  certificate or by answering on that certificate when hither connects back.
- Quick Share uses the UKEY2 handshake and shows its PIN in the prompt so you can
  compare it with the phone.
- Received files never overwrite anything. Names are cleaned, and each file is
  written to a fresh `.part` file and renamed when it is complete.
- The local control socket and every key file are readable only by your user.
  The Mac app only acts on `hither://` links that carry a secret from the data
  folder, so web pages and other apps cannot trigger it.
- The Android app accepts clipboard writes only from adb.

To report a security problem, please open a
[private advisory](https://github.com/ArinFaraj/hither/security/advisories/new)
instead of an issue.

## How it works

- Phones are reached through adb. hither starts a small helper on the phone with
  `app_process` to watch the clipboard, so nothing has to be installed for that
  part.
- LocalSend is the open v2 protocol.
- Quick Share is a clean room implementation written from the protocol notes in
  [NearDrop](https://github.com/grishka/NearDrop) (Unlicense).
- AirDrop is not reimplemented. On macOS hither uses the system sharing service.

## Building

You need Rust 1.85 or newer.

```sh
cargo build --release
./target/release/hither autostart on
```

`hither autostart on` installs a launchd agent on macOS or a systemd user unit on
Linux so the service starts at login. On macOS, build the menu bar app with
`./scripts/build-mac-app.sh`, which writes `target/Hither.app`.

The Android app and the phone helper are built without Gradle, using only the
Android SDK: `./android/build.sh` and `./helper/build.sh` write
`assets/hither.apk` and `assets/hither-helper.jar`, which get embedded in the
binary. `./scripts/build-linux-arm64.sh` cross compiles a static Linux arm64
binary from a Mac with the clang from the Android NDK.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.
