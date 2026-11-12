import AppKit
import ServiceManagement
import SwiftUI
import UniformTypeIdentifiers
import UserNotifications

struct Device: Identifiable, Hashable {
    let id: String
    let name: String
    let via: [String]
    let clip: Bool
    let phone: Bool
    let foreign: Bool

    var airdrop: Bool { id == AirDrop.id }

    static let airdropRow = Device(id: AirDrop.id, name: "AirDrop", via: ["airdrop"], clip: true, phone: false, foreign: false)
}

enum AirDrop {
    static let id = "airdrop"

    static var available: Bool { NSSharingService(named: .sendViaAirDrop) != nil }

    @MainActor
    static func share(_ items: [Any]) -> Bool {
        guard let s = NSSharingService(named: .sendViaAirDrop), s.canPerform(withItems: items) else { return false }
        NSApp.activate(ignoringOtherApps: true)
        s.perform(withItems: items)
        return true
    }

    @MainActor
    static func shareClipboard() -> String? {
        let pb = NSPasteboard.general
        if let text = pb.string(forType: .string), !text.isEmpty {
            let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
            if !trimmed.contains(where: \.isWhitespace), let url = URL(string: trimmed), url.scheme == "http" || url.scheme == "https" {
                if share([url]) { return nil }
            }
            if share([text]) { return nil }
            guard let file = temp("Clipboard.txt", Data(text.utf8)) else { return "Could not save the text" }
            return share([file]) ? nil : "AirDrop can't send this"
        }
        if let img = NSImage(pasteboard: pb), let tiff = img.tiffRepresentation,
           let png = NSBitmapImageRep(data: tiff)?.representation(using: .png, properties: [:]) {
            guard let file = temp("Clipboard.png", png) else { return "Could not save the image" }
            return share([file]) ? nil : "AirDrop can't send this"
        }
        return "Clipboard is empty"
    }

    private static func temp(_ name: String, _ data: Data) -> URL? {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("hither-airdrop", isDirectory: true)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        let url = dir.appendingPathComponent(name)
        return (try? data.write(to: url)) == nil ? nil : url
    }
}

enum Notifier {
    static func post(_ url: URL) {
        let q = URLComponents(url: url, resolvingAgainstBaseURL: false)?.queryItems ?? []
        func value(_ k: String) -> String? { q.first { $0.name == k }?.value }
        let content = UNMutableNotificationContent()
        content.title = value("title") ?? "Hither"
        content.body = value("body") ?? ""
        if let f = value("file") { content.userInfo = ["file": f] }
        if let f = value("image") ?? value("file"), let a = attachment(URL(fileURLWithPath: f)) {
            content.attachments = [a]
        }
        UNUserNotificationCenter.current().add(UNNotificationRequest(identifier: UUID().uuidString, content: content, trigger: nil))
    }

    private static func attachment(_ src: URL) -> UNNotificationAttachment? {
        guard let type = UTType(filenameExtension: src.pathExtension), type.conforms(to: .image) else { return nil }
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("hither-notify", isDirectory: true)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        let copy = dir.appendingPathComponent(UUID().uuidString).appendingPathExtension(src.pathExtension)
        guard (try? FileManager.default.copyItem(at: src, to: copy)) != nil else { return nil }
        return try? UNNotificationAttachment(identifier: "image", url: copy)
    }
}

final class AppDelegate: NSObject, NSApplicationDelegate, UNUserNotificationCenterDelegate {
    func applicationWillFinishLaunching(_ notification: Notification) {
        let c = UNUserNotificationCenter.current()
        c.delegate = self
        c.requestAuthorization(options: [.alert, .sound]) { _, _ in }
    }

    func userNotificationCenter(_ center: UNUserNotificationCenter, willPresent notification: UNNotification) async -> UNNotificationPresentationOptions {
        [.banner, .list]
    }

    func userNotificationCenter(_ center: UNUserNotificationCenter, didReceive response: UNNotificationResponse) async {
        guard let f = response.notification.request.content.userInfo["file"] as? String else { return }
        let url = URL(fileURLWithPath: f)
        await MainActor.run {
            if FileManager.default.fileExists(atPath: f) { NSWorkspace.shared.activateFileViewerSelecting([url]) }
        }
    }

    private static func signed(_ u: URL) -> Bool {
        let home = ProcessInfo.processInfo.environment["HITHER_HOME"].map { URL(fileURLWithPath: $0) }
            ?? FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent("Library/Application Support/hither")
        guard let want = try? String(contentsOf: home.appendingPathComponent("menu-key"), encoding: .utf8)
            .trimmingCharacters(in: .whitespacesAndNewlines), !want.isEmpty else { return false }
        let got = URLComponents(url: u, resolvingAgainstBaseURL: false)?.queryItems?.first { $0.name == "key" }?.value
        return got == want
    }

    func application(_ application: NSApplication, open urls: [URL]) {
        let commands = urls.filter { $0.scheme == "hither" && Self.signed($0) }
        for u in commands where u.host == "notify" { Notifier.post(u) }
        if commands.contains(where: { $0.host == "airdrop-clipboard" }) {
            Task { @MainActor in
                if let err = AirDrop.shareClipboard() {
                    let a = NSAlert()
                    a.messageText = err
                    NSApp.activate(ignoringOtherApps: true)
                    a.runModal()
                }
            }
            return
        }
        let files = urls.filter(\.isFileURL)
        if !files.isEmpty { _ = AirDrop.share(files) }
    }
}

struct Event: Identifiable {
    let id = UUID()
    let kind: String
    let device: String
    let detail: String
    let at: Date
}

enum CLI {
    static let path: String = {
        if let u = Bundle.main.url(forAuxiliaryExecutable: "hither") { return u.path }
        return NSHomeDirectory() + "/bin/hither"
    }()

    static func run(_ args: [String], input: String? = nil) async -> [String: Any] {
        await Task.detached {
            let p = Process()
            p.executableURL = URL(fileURLWithPath: path)
            p.arguments = args + ["--json"]
            let out = Pipe()
            p.standardOutput = out
            p.standardError = FileHandle.nullDevice
            let inp = Pipe()
            p.standardInput = input == nil ? FileHandle.nullDevice : inp
            do { try p.run() } catch { return ["ok": false, "error": error.localizedDescription] }
            if let input {
                inp.fileHandleForWriting.write(Data(input.utf8))
                try? inp.fileHandleForWriting.close()
            }
            let data = out.fileHandleForReading.readDataToEndOfFile()
            p.waitUntilExit()
            return (try? JSONSerialization.jsonObject(with: data)) as? [String: Any]
                ?? ["ok": false, "error": "hither did not answer"]
        }.value
    }
}

@MainActor
final class Model: ObservableObject {
    @Published var name = ""
    @Published var clip = true
    @Published var inbox = ""
    @Published var devices: [Device] = []
    @Published var recent: [Event] = []
    @Published var status: [String: String] = [:]
    @Published var ticket = ""
    @Published var pairMessage = ""
    @Published var loaded = false
    @Published var update: String?
    @Published var updating = false
    @Published var updateError: String?

    func refresh() async {
        let s = await CLI.run(["status"])
        let d = await CLI.run(["devices"])
        let r = await CLI.run(["recent"])
        name = s["name"] as? String ?? ""
        clip = s["clip"] as? Bool ?? false
        inbox = s["inbox"] as? String ?? ""
        update = s["update"] as? String
        var list = (d["devices"] as? [[String: Any]] ?? []).map {
            let serial = $0["serial"] as? String
            let ls = $0["localsend"] as? String
            return Device(
                id: serial ?? $0["id"] as? String ?? ls ?? $0["name"] as? String ?? "",
                name: $0["name"] as? String ?? "",
                via: $0["via"] as? [String] ?? [],
                clip: $0["clip"] as? Bool ?? true,
                phone: serial != nil || $0["type"] as? String == "mobile",
                foreign: serial == nil && !($0["id"] is String) && (ls != nil || $0["quickshare"] is String)
            )
        }
        if AirDrop.available { list.append(.airdropRow) }
        devices = list
        recent = (r["recent"] as? [[String: Any]] ?? []).prefix(5).map {
            Event(
                kind: $0["kind"] as? String ?? "",
                device: $0["device"] as? String ?? "",
                detail: $0["detail"] as? String ?? "",
                at: Date(timeIntervalSince1970: $0["at"] as? Double ?? 0)
            )
        }
        loaded = true
    }

    func installUpdate() async {
        updating = true
        let r = await CLI.run(["update"])
        updating = false
        updateError = r["ok"] as? Bool == true ? nil : r["error"] as? String ?? "Update failed"
        await refresh()
    }

    func flash(_ d: Device, _ text: String) {
        status[d.id] = text
        Task {
            try? await Task.sleep(for: .seconds(4))
            if status[d.id] == text { status[d.id] = nil }
        }
    }

    func send(_ d: Device, _ urls: [URL]) async {
        guard !urls.isEmpty else { return }
        if d.airdrop {
            if !AirDrop.share(urls) { flash(d, "AirDrop can't send these") }
            return
        }
        status[d.id] = urls.count == 1 ? "Sending \(urls[0].lastPathComponent)" : "Sending \(urls.count) files"
        let r = await CLI.run(["send", d.id] + urls.map(\.path))
        flash(d, r["ok"] as? Bool == true ? "Sent" : r["error"] as? String ?? "Failed")
        await refresh()
    }

    func sendClipboard(_ d: Device) async {
        if d.airdrop {
            if let err = AirDrop.shareClipboard() { flash(d, err) }
            return
        }
        guard let text = NSPasteboard.general.string(forType: .string) else { return flash(d, "Clipboard is empty") }
        let r = await CLI.run(["clip", "send", d.id, "-"], input: text)
        flash(d, r["ok"] as? Bool == true ? "Clipboard sent" : r["error"] as? String ?? "Failed")
    }

    func setClip(_ on: Bool, for d: Device? = nil) async {
        _ = await CLI.run(["clip", on ? "on" : "off"] + (d.map { [$0.id] } ?? []))
        await refresh()
    }

    func forget(_ d: Device) async {
        _ = await CLI.run(["forget", d.id])
        await refresh()
    }

    func startPair() async {
        pairMessage = ""
        let r = await CLI.run(["pair"])
        ticket = r["ticket"] as? String ?? ""
    }

    func join(_ t: String) async -> Bool {
        let r = await CLI.run(["pair", t.trimmingCharacters(in: .whitespacesAndNewlines)])
        if r["ok"] as? Bool == true {
            pairMessage = "Paired with \(r["device"] as? String ?? "device")"
            await refresh()
            return true
        }
        pairMessage = r["error"] as? String ?? "Pairing failed"
        return false
    }
}

struct Panel: View {
    @ObservedObject var m: Model
    @State private var pairing = false
    @State private var joinText = ""
    @State private var atLogin = SMAppService.mainApp.status == .enabled
    private let tick = Timer.publish(every: 3, on: .main, in: .common).autoconnect()

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack {
                Text(m.name.isEmpty ? "Hither" : m.name).font(.headline)
                Spacer()
                Toggle("Clipboard", isOn: Binding(get: { m.clip }, set: { v in Task { await m.setClip(v) } }))
                    .toggleStyle(.switch)
                    .controlSize(.mini)
                    .font(.caption)
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 10)
            Divider()
            if pairing { pairView } else { mainView }
            Divider()
            footer
        }
        .frame(width: 300)
        .task { await m.refresh() }
        .onReceive(tick) { _ in
            if NSApp.keyWindow?.isVisible == true && !pairing { Task { await m.refresh() } }
        }
        .onReceive(NotificationCenter.default.publisher(for: NSWindow.didBecomeKeyNotification)) { _ in
            Task { await m.refresh() }
        }
    }

    private var mainView: some View {
        VStack(alignment: .leading, spacing: 0) {
            if let v = m.update {
                HStack {
                    Text(m.updating ? "Updating to \(v)..." : m.updateError ?? "Hither \(v) is out")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    Spacer()
                    Button("Update") { Task { await m.installUpdate() } }
                        .buttonStyle(.borderless)
                        .font(.caption)
                        .disabled(m.updating)
                }
                .padding(.horizontal, 14)
                .padding(.vertical, 8)
                Divider()
            }
            if m.devices.isEmpty {
                Text(m.loaded ? "No devices yet. Pair one, or plug in a phone with USB debugging on." : "Starting...")
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .padding(14)
            } else {
                VStack(spacing: 2) {
                    ForEach(m.devices) { DeviceRow(d: $0, m: m) }
                }
                .padding(.vertical, 6)
                Text("Drop files on a device to send. Right click for more.")
                    .font(.caption2)
                    .foregroundStyle(.tertiary)
                    .padding(.horizontal, 14)
                    .padding(.bottom, 8)
            }
            if !m.recent.isEmpty {
                Divider()
                VStack(alignment: .leading, spacing: 6) {
                    ForEach(m.recent) { RecentRow(e: $0) }
                }
                .padding(14)
            }
        }
    }

    private var pairView: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("On the other device, run this or paste it there:")
                .font(.callout)
            HStack {
                Text(m.ticket.isEmpty ? "..." : m.ticket)
                    .font(.system(.caption, design: .monospaced))
                    .lineLimit(1)
                    .truncationMode(.middle)
                    .textSelection(.enabled)
                Spacer()
                Button("Copy") {
                    NSPasteboard.general.clearContents()
                    NSPasteboard.general.setString("hither pair \(m.ticket)", forType: .string)
                }
                .disabled(m.ticket.isEmpty)
            }
            Text("Or join a device that showed you a ticket:")
                .font(.callout)
                .padding(.top, 4)
            HStack {
                TextField("ticket", text: $joinText)
                    .textFieldStyle(.roundedBorder)
                    .font(.system(.caption, design: .monospaced))
                Button("Join") {
                    let t = joinText.replacingOccurrences(of: "hither pair", with: "")
                    Task { if await m.join(t) { joinText = "" } }
                }
                .disabled(joinText.isEmpty)
            }
            if !m.pairMessage.isEmpty {
                Text(m.pairMessage).font(.caption).foregroundStyle(.secondary)
            }
        }
        .padding(14)
    }

    private var footer: some View {
        HStack {
            Button(pairing ? "Done" : "Pair a device") {
                pairing.toggle()
                if pairing { Task { await m.startPair() } }
            }
            Spacer()
            Menu {
                Button("Open inbox") { NSWorkspace.shared.open(URL(fileURLWithPath: m.inbox)) }
                Toggle("Open at login", isOn: Binding(get: { atLogin }, set: { v in
                    try? v ? SMAppService.mainApp.register() : SMAppService.mainApp.unregister()
                    atLogin = SMAppService.mainApp.status == .enabled
                }))
                Divider()
                Button("Quit") { NSApp.terminate(nil) }
            } label: {
                Image(systemName: "ellipsis.circle")
            }
            .menuStyle(.borderlessButton)
            .menuIndicator(.hidden)
            .fixedSize()
        }
        .buttonStyle(.borderless)
        .padding(.horizontal, 14)
        .padding(.vertical, 8)
    }
}

struct DeviceRow: View {
    let d: Device
    @ObservedObject var m: Model
    @State private var hover = false
    @State private var target = false

    private var online: Bool { !d.via.isEmpty }

    private var route: String {
        guard online else { return "Offline" }
        return d.via.map { v in
            if v == "usb" { return "USB" }
            if v == "net" { return "Network" }
            if v.hasPrefix("adb ") { return "Wi-Fi" }
            if v.hasPrefix("lan") { return "LAN" }
            if v.hasPrefix("tailscale") { return "Tailscale" }
            if v.hasPrefix("direct") { return "Internet" }
            if v.hasPrefix("relay") { return "Internet (relay)" }
            if v.hasPrefix("localsend") { return "LocalSend" }
            if v.hasPrefix("quickshare") { return "Quick Share" }
            if v == "airdrop" { return "Nearby Apple devices" }
            return v
        }.joined(separator: ", ")
    }

    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: d.airdrop ? "dot.radiowaves.left.and.right" : d.phone ? "iphone" : "laptopcomputer")
                .frame(width: 18)
                .foregroundStyle(.secondary)
            VStack(alignment: .leading, spacing: 1) {
                Text(d.name)
                Text(m.status[d.id] ?? route)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
            Spacer()
            if hover && online {
                Button { Task { await m.sendClipboard(d) } } label: { Image(systemName: "doc.on.clipboard.fill") }
                    .buttonStyle(.borderless)
                    .help("Send the clipboard to \(d.name)")
            } else if !d.clip && !d.foreign && !d.airdrop {
                Image(systemName: "doc.on.clipboard")
                    .foregroundStyle(.tertiary)
                    .help("Clipboard sync is off for this device")
            }
        }
        .opacity(online ? 1 : 0.5)
        .padding(.horizontal, 10)
        .padding(.vertical, 6)
        .background(
            RoundedRectangle(cornerRadius: 6)
                .fill(target ? Color.accentColor.opacity(0.25) : hover ? Color.primary.opacity(0.07) : .clear)
        )
        .padding(.horizontal, 6)
        .contentShape(Rectangle())
        .onHover { hover = $0 }
        .onTapGesture { pick() }
        .onDrop(of: [UTType.fileURL], isTargeted: $target) { providers in
            load(providers)
            return true
        }
        .contextMenu {
            Button("Send files...") { pick() }
            Button("Send clipboard") { Task { await m.sendClipboard(d) } }
            if !d.foreign && !d.airdrop {
                Toggle("Sync clipboard", isOn: Binding(get: { d.clip }, set: { v in Task { await m.setClip(v, for: d) } }))
                Divider()
                Button("Forget") { Task { await m.forget(d) } }
            }
        }
        .help("Click to pick files, or drop files here")
    }

    private func pick() {
        let panel = NSOpenPanel()
        panel.allowsMultipleSelection = true
        panel.canChooseDirectories = false
        panel.message = "Send to \(d.name)"
        panel.prompt = "Send"
        NSApp.activate(ignoringOtherApps: true)
        if panel.runModal() == .OK {
            let urls = panel.urls
            Task { await m.send(d, urls) }
        }
    }

    private func load(_ providers: [NSItemProvider]) {
        let group = DispatchGroup()
        let lock = NSLock()
        var urls: [URL] = []
        for p in providers {
            group.enter()
            _ = p.loadObject(ofClass: URL.self) { url, _ in
                if let url {
                    lock.lock()
                    urls.append(url)
                    lock.unlock()
                }
                group.leave()
            }
        }
        group.notify(queue: .main) {
            Task { await m.send(d, urls) }
        }
    }
}

struct RecentRow: View {
    let e: Event

    private var text: String {
        let file = URL(fileURLWithPath: e.detail).lastPathComponent
        switch e.kind {
        case "file_in": return "\(file) from \(e.device)"
        case "file_out": return "\(file) to \(e.device)"
        case "clip_in": return "Clipboard from \(e.device)"
        case "paired": return "Paired with \(e.device)"
        default: return "\(e.kind) \(e.device)"
        }
    }

    private var icon: String {
        switch e.kind {
        case "file_in": return "arrow.down"
        case "file_out": return "arrow.up"
        case "clip_in": return "doc.on.clipboard"
        default: return "link"
        }
    }

    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: icon).frame(width: 14).foregroundStyle(.secondary)
            Text(text).lineLimit(1).truncationMode(.middle)
            Spacer()
            Text(e.at, format: .relative(presentation: .numeric, unitsStyle: .narrow))
                .foregroundStyle(.tertiary)
        }
        .font(.caption)
        .contentShape(Rectangle())
        .onTapGesture {
            if e.kind == "file_in" {
                NSWorkspace.shared.activateFileViewerSelecting([URL(fileURLWithPath: e.detail)])
            }
        }
    }
}

@main
struct HitherApp: App {
    static let mark: NSImage = {
        let img = NSImage(size: NSSize(width: 18, height: 18), flipped: true) { _ in
            let u: CGFloat = 18 / 50
            func p(_ x: CGFloat, _ y: CGFloat) -> NSPoint { NSPoint(x: (x - 29) * u, y: (y - 29) * u) }
            let arrows = NSBezierPath()
            for c in [[(44, 32), (54, 42), (64, 32)], [(44, 76), (54, 66), (64, 76)],
                      [(32, 44), (42, 54), (32, 64)], [(76, 44), (66, 54), (76, 64)]] {
                arrows.move(to: p(CGFloat(c[0].0), CGFloat(c[0].1)))
                arrows.line(to: p(CGFloat(c[1].0), CGFloat(c[1].1)))
                arrows.line(to: p(CGFloat(c[2].0), CGFloat(c[2].1)))
            }
            arrows.lineWidth = 2
            arrows.lineCapStyle = .round
            arrows.lineJoinStyle = .round
            NSColor.black.setStroke()
            arrows.stroke()
            let r = 7 * u
            let c = p(54, 54)
            NSColor.black.setFill()
            NSBezierPath(ovalIn: NSRect(x: c.x - r, y: c.y - r, width: 2 * r, height: 2 * r)).fill()
            return true
        }
        img.isTemplate = true
        return img
    }()

    @NSApplicationDelegateAdaptor(AppDelegate.self) private var delegate
    @StateObject private var m = Model()

    var body: some Scene {
        MenuBarExtra {
            Panel(m: m)
        } label: {
            Image(nsImage: HitherApp.mark)
        }
        .menuBarExtraStyle(.window)
    }
}
