// Spot Launcher: ⌥Space from anywhere opens a Spotlight-style bar that searches Spotify through
// the running spot (over its socket, ~/.cache/spot/sock) and plays what you pick.
// ↑↓ move, ⏎ plays, ⌘⏎ queues, ⌘C copies a link (with no results, to what's playing), tab cycles songs / albums / artists, ⌃S shuffle, ⌃R repeat, esc closes.

import AppKit
import Carbon.HIToolbox
import Combine
import ServiceManagement
import SwiftUI

// MARK: talking to spot

/// A connection to the running spot, if there is one.
func connectSpot() -> Int32? {
    let path = NSHomeDirectory() + "/.cache/spot/sock"
    let fd = socket(AF_UNIX, SOCK_STREAM, 0)
    guard fd >= 0 else { return nil }
    var addr = sockaddr_un()
    addr.sun_family = sa_family_t(AF_UNIX)
    withUnsafeMutableBytes(of: &addr.sun_path) { buf in
        path.utf8CString.withUnsafeBytes { buf.copyMemory(from: UnsafeRawBufferPointer(rebasing: $0.prefix(buf.count - 1))) }
    }
    let ok = withUnsafePointer(to: &addr) {
        $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { connect(fd, $0, socklen_t(MemoryLayout<sockaddr_un>.size)) }
    }
    guard ok == 0 else {
        close(fd)
        return nil
    }
    return fd
}

/// One request to the running spot: the reply's exit code and text. Code 3: it isn't running.
func spot(_ args: [String]) -> (Int, String) {
    guard let fd = connectSpot() else { return (3, "spot isn't running") }
    defer { close(fd) }
    let line = Array((args.joined(separator: "\t") + "\n").utf8)
    guard write(fd, line, line.count) == line.count else { return (1, "couldn't send the request") }
    var data = Data()
    var buf = [UInt8](repeating: 0, count: 65536)
    while true {
        let n = read(fd, &buf, buf.count)
        if n <= 0 { break }
        data.append(buf, count: n)
    }
    let reply = String(decoding: data, as: UTF8.self)
    let (head, rest) = reply.firstIndex(of: "\n").map { (String(reply[..<$0]), String(reply[reply.index(after: $0)...])) } ?? (reply, "")
    guard let code = Int(head) else { return (1, "no reply from spot (an older version? `spot quit`, then start it again)") }
    return (code, rest.trimmingCharacters(in: .newlines))
}

/// Follow `spot watch` for good, calling `onSkip` per media key press. Blocks, so it gets a thread
/// of its own; while spot isn't running it looks again every few seconds.
func watchSkips(_ onSkip: @escaping @Sendable () -> Void) {
    while true {
        if let fd = connectSpot() {
            var pending = Data()
            var buf = [UInt8](repeating: 0, count: 4096)
            if write(fd, "watch\n", 6) == 6 {
                while case let n = read(fd, &buf, buf.count), n > 0 {
                    pending.append(buf, count: n)
                    while let end = pending.firstIndex(of: 10) {
                        if pending[pending.startIndex..<end].starts(with: "skip:".utf8) { onSkip() }
                        pending = Data(pending[(end + 1)...])
                    }
                }
            }
            close(fd)
        }
        sleep(3)
    }
}

/// `spot start`: the player in the background. Looks where install.sh and cargo put it, then
/// asks your shell (an app doesn't get its PATH).
func startSpot() -> Bool {
    let home = NSHomeDirectory()
    let candidates = ["\(home)/.local/bin/spot", "\(home)/.cargo/bin/spot", "/opt/homebrew/bin/spot", "/usr/local/bin/spot"]
    guard let exe = candidates.first(where: { FileManager.default.isExecutableFile(atPath: $0) }) ?? fromShell("spot") else { return false }
    let p = Process()
    p.executableURL = URL(fileURLWithPath: exe)
    p.arguments = ["start"]
    p.standardOutput = FileHandle.nullDevice
    p.standardError = FileHandle.nullDevice
    guard (try? p.run()) != nil else { return false }
    p.waitUntilExit()
    return p.terminationStatus == 0
}

/// Where your login shell finds `cmd`, if anywhere.
func fromShell(_ cmd: String) -> String? {
    let p = Process()
    p.executableURL = URL(fileURLWithPath: ProcessInfo.processInfo.environment["SHELL"] ?? "/bin/zsh")
    p.arguments = ["-lic", "command -v \(cmd)"]
    let out = Pipe()
    p.standardOutput = out
    p.standardError = FileHandle.nullDevice
    p.standardInput = FileHandle.nullDevice
    guard (try? p.run()) != nil else { return nil }
    p.waitUntilExit()
    let path = String(decoding: out.fileHandleForReading.readDataToEndOfFile(), as: UTF8.self)
        .split(separator: "\n").last.map(String.init)?.trimmingCharacters(in: .whitespaces)
    return path.flatMap { FileManager.default.isExecutableFile(atPath: $0) ? $0 : nil }
}

// MARK: model

/// spotify:track:id as https://open.spotify.com/track/id
func shareLink(_ uri: String) -> String {
    "https://open.spotify.com/" + uri.dropFirst("spotify:".count).replacingOccurrences(of: ":", with: "/")
}

/// What a search finds, cycled with tab, as in spot's own `/` prompt.
enum Kind: String, CaseIterable {
    case songs, albums, artists
    var flag: String { "--\(rawValue)" }
}

struct Result: Identifiable {
    let uri, name, sub: String
    let ms: Int
    let explicit: Bool
    var id: String { uri }

    init?(_ line: Substring) {
        let f = line.split(separator: "\t", omittingEmptySubsequences: false).map(String.init)
        guard f.count >= 6 else { return nil }
        (uri, name, sub) = (f[0], f[1], f[2])
        ms = Int(f[4]) ?? 0
        explicit = f[5] == "1"
    }

    var duration: String { ms > 0 ? String(format: "%d:%02d", ms / 60000, ms / 1000 % 60) : "" }
}

/// What spot is playing, from `spot status --tsv`.
struct Playing: Equatable {
    var playing: Bool
    let name, artists, uri: String
    var shuffle: Bool
    var repeats: String  // off, all, one
}

@MainActor
final class Model: ObservableObject {
    @Published var query = "" { didSet { if query != oldValue { search() } } }
    @Published var kind = Kind.songs { didSet { search() } }
    @Published var results: [Result] = []
    @Published var selected = 0
    @Published var note: String?  // an error or "starting spot…", shown in place of results
    @Published var busy = false
    let theme = Theme()
    var popup = true  // config.toml, by way of spot
    var popupSeconds = 3.0
    @Published var now: Playing?
    @Published var copied = false  // a link just went to the clipboard
    var close: () -> Void = {}
    private var generation = 0
    private var copiedFade: Task<Void, Never>?

    func reset() {
        query = ""
        results = []
        note = nil
        selected = 0
        copied = false
    }

    /// Search as you type: waits a moment for typing to stop, and drops answers to older queries.
    func search() {
        generation += 1
        let gen = generation
        let q = query.trimmingCharacters(in: .whitespaces)
        guard !q.isEmpty else {
            results = []
            note = nil
            busy = false
            return
        }
        busy = true
        let kind = kind
        Task {
            try? await Task.sleep(for: .milliseconds(140))
            guard gen == generation else { return }
            let (code, text) = await request(["search", "--tsv", "-n", "\(maxRows)", kind.flag, q])
            guard gen == generation else { return }
            busy = false
            if code == 0 && !text.isEmpty && !text.contains("\t") {
                results = []
                note = "the running spot is too old for this: quit it (q) and start it again"
            } else if code == 0 {
                results = text.split(separator: "\n").compactMap(Result.init)
                note = results.isEmpty ? "no results" : nil
                selected = 0
            } else {
                results = []
                note = text
            }
        }
    }

    /// Ask spot what's playing; nothing if it isn't running (this doesn't start it).
    func fetchNow() async {
        let (code, text) = await Task.detached { spot(["status", "--tsv"]) }.value
        let f = text.split(separator: "\t", omittingEmptySubsequences: false).map(String.init)
        let now = code == 0 && f.count >= 7
            ? Playing(playing: f[0] == "1", name: f[1], artists: f[2], uri: f[4], shuffle: f[5] == "1", repeats: f[6]) : nil
        if now != self.now { self.now = now }
    }

    func refreshNow() { Task { await fetchNow() } }

    /// The popup's settings; the defaults if spot is too old to say.
    func fetchPopup() async {
        let (code, text) = await Task.detached { spot(["launcher"]) }.value
        let f = text.split(separator: "\t", omittingEmptySubsequences: false)
        guard code == 0, f.count >= 2, let seconds = Double(f[1]) else { return }
        (popup, popupSeconds) = (f[0] == "1", seconds)
    }

    func togglePlay() { control(["toggle"]) { $0.playing.toggle() } }
    func toggleShuffle() { control(["shuffle"]) { $0.shuffle.toggle() } }
    /// off → all → one, as spot's `r` does.
    func cycleRepeat() { control(["repeat"]) { $0.repeats = ["off": "all", "all": "one"][$0.repeats] ?? "off" } }

    /// Show the change right away, then tell spot and check what it did.
    private func control(_ args: [String], _ change: (inout Playing) -> Void) {
        guard var n = now else { return }
        change(&n)
        now = n
        Task {
            _ = await Task.detached { spot(args) }.value
            refreshNow()
        }
    }

    func move(_ by: Int) {
        guard !results.isEmpty else { return }
        selected = (selected + by + results.count) % results.count
    }

    func cycleKind(back: Bool) {
        let all = Kind.allCases
        kind = all[(all.firstIndex(of: kind)! + (back ? all.count - 1 : 1)) % all.count]
    }

    /// Play (or queue) the selected result, then get out of the way.
    /// Before the results are in, the window closes right away and the first result plays (or
    /// queues) in the background once it's found; a beep if nothing is.
    func choose(queue: Bool) {
        if busy {
            let (q, kind) = (query.trimmingCharacters(in: .whitespaces), kind)
            generation += 1  // the window's own search is moot now
            busy = false
            close()
            Task {
                let (code, text) = await request(["search", "--tsv", "-n", "\(maxRows)", kind.flag, q])
                guard code == 0, let first = text.split(separator: "\n").first.flatMap(Result.init) else { return NSSound.beep() }
                if await request(queue ? ["queue", "add", first.uri] : ["play", first.uri]).0 != 0 { NSSound.beep() }
            }
            return
        }
        guard results.indices.contains(selected) else { return }
        let uri = results[selected].uri
        Task {
            let (code, text) = await request(queue ? ["queue", "add", uri] : ["play", uri])
            if code == 0 { close() } else { note = text }
        }
    }

    /// ⌘C: the link to the selected result, or with no results what's playing, on the clipboard,
    /// and a moment's "copied".
    func copyLink() {
        guard let uri = results.indices.contains(selected) ? results[selected].uri : now?.uri, !uri.isEmpty else { return }
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(shareLink(uri), forType: .string)
        copied = true
        copiedFade?.cancel()
        copiedFade = Task {
            try? await Task.sleep(for: .seconds(1.2))
            if !Task.isCancelled { copied = false }
        }
    }

    /// Off the main thread; starts spot first if it isn't running.
    private func request(_ args: [String]) async -> (Int, String) {
        let first = await Task.detached { spot(args) }.value
        guard first.0 == 3 else { return first }
        note = "starting spot…"
        let started = await Task.detached { startSpot() }.value
        guard started else { return (3, "spot isn't running, and `spot start` didn't start it") }
        note = nil
        return await Task.detached { spot(args) }.value
    }
}

// MARK: theme

/// The system's colors: on glass the text takes its label colors, which stay readable over
/// whatever is behind, and the accent is the one set in System Settings.
struct Theme {
    let accent = Color.accentColor
    let hl = Color.accentColor.opacity(0.22)
    let text = Color.primary
    let sub = Color.secondary
    let dim = Color(nsColor: .tertiaryLabelColor)
    let gold = Color.accentColor
    let err = Color.red
}

// MARK: views

let width: CGFloat = 540
let rowHeight: CGFloat = 22
let barHeight: CGFloat = 30
let nowHeight: CGFloat = 26
let maxRows = 12
let toastWidth: CGFloat = 340  // longer than this ends in …
let toastGap: CGFloat = 6  // between the notch and the toast
let mono = Font.system(size: 13, design: .monospaced)

/// spot's status line and list, floating: results numbered above, the search line at the bottom.
struct LauncherView: View {
    @ObservedObject var model: Model
    @FocusState private var focused: Bool

    var body: some View {
        let th = model.theme
        VStack(spacing: 0) {
            if let note = model.note {
                Text(note)
                    .foregroundStyle(model.note == "no results" || model.note == "starting spot…" ? th.sub : th.err)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.horizontal, 12)
                    .frame(height: barHeight)
            } else if !model.results.isEmpty {
                VStack(spacing: 0) {
                    ForEach(Array(model.results.enumerated()), id: \.element.id) { i, r in
                        // the list's margin belongs to its first and last rows, so their highlight
                        // reaches the window's top edge and the rule
                        let margin: (top: CGFloat, bottom: CGFloat) = (i == 0 ? 6 : 0, i == model.results.count - 1 ? 6 : 0)
                        Row(n: i + 1, result: r, songs: model.kind == .songs, selected: i == model.selected, margin: margin, th: th)
                            .contentShape(Rectangle())
                            .onTapGesture { model.selected = i; model.choose(queue: false) }
                    }
                }
            }
            if model.note != nil || !model.results.isEmpty {
                Rectangle().fill(.separator).frame(height: 1)
            }
            Prompt(model: model, focused: $focused, th: th)
            if let now = model.now {
                NowPlaying(model: model, now: now, th: th)
            }
        }
        .font(mono)
        .frame(width: width)
        .modifier(Glass(th: th))
        .padding(margin)
        // in the window's margin under the glass, so showing it doesn't resize anything
        .overlay(alignment: .bottom) {
            if model.copied {
                Text("copied")
                    .font(.system(size: 11, weight: .medium))
                    .foregroundStyle(.white)
                    .padding(.horizontal, 9)
                    .padding(.vertical, 3)
                    .background(th.accent, in: Capsule())
                    .padding(.bottom, 12)
                    .transition(.opacity)
            }
        }
        .animation(.easeOut(duration: 0.15), value: model.copied)
        .onAppear { focused = true }
    }
}

/// Liquid Glass over a partial blur of what's behind (so text under the window doesn't fight the
/// results), tinted faintly with the theme's accent; before macOS 26, the blur alone. The glass
/// follows System Settings (Appearance → Liquid Glass, and Reduce Transparency).
let glassBlur = 0.5  // how much blur under the glass: 1 frosted and plain, 0 all glass
struct Glass: ViewModifier {
    let th: Theme

    func body(content: Content) -> some View {
        let shape = RoundedRectangle(cornerRadius: corner, style: .continuous)
        // glass needs the macOS 26 SDK (Xcode 26) to build; older ones build the blur alone
        #if compiler(>=6.2)
        if #available(macOS 26, *) {
            content.background(Blur().opacity(glassBlur)).clipShape(shape)
                .glassEffect(.regular.tint(th.accent.opacity(0.06)).interactive(), in: shape)
        } else {
            content.background(Blur()).clipShape(shape).overlay(shape.strokeBorder(.separator))
        }
        #else
        content.background(Blur()).clipShape(shape).overlay(shape.strokeBorder(.separator))
        #endif
    }
}

/// Dragging here moves the window (macOS 15+; before that, only its empty background does).
struct DragsWindow: ViewModifier {
    func body(content: Content) -> some View {
        if #available(macOS 15, *) {
            content.contentShape(Rectangle()).gesture(WindowDragGesture())
        } else {
            content
        }
    }
}

let corner: CGFloat = 14
/// Transparent room around the glass, inside the window: the glass's shadow (stronger while the
/// window is key) fades out there instead of being cut off square by the window's edge.
let margin: CGFloat = 40

struct Blur: NSViewRepresentable {
    func makeNSView(context: Context) -> NSVisualEffectView {
        let v = NSVisualEffectView()
        v.material = .hudWindow
        v.blendingMode = .behindWindow
        v.state = .active
        // the window server blurs the view's whole rectangle, whatever SwiftUI clips; only a mask
        // rounds it (else its square corners show past the glass)
        let mask = NSImage(size: NSSize(width: corner * 2 + 1, height: corner * 2 + 1), flipped: false) { r in
            NSBezierPath(roundedRect: r, xRadius: corner, yRadius: corner).fill()
            return true
        }
        mask.capInsets = NSEdgeInsets(top: corner, left: corner, bottom: corner, right: corner)
        mask.resizingMode = .stretch
        v.maskImage = mask
        return v
    }
    func updateNSView(_ v: NSVisualEffectView, context: Context) {}
}

/// `▶ name  artists`, like the top line of spot's player
struct NowPlaying: View {
    @ObservedObject var model: Model
    let now: Playing
    let th: Theme

    var body: some View {
        HStack(spacing: 8) {
            icon(now.playing ? "pause.fill" : "play.fill", on: true, help: now.playing ? "Pause" : "Play", model.togglePlay)
            Text(now.name).foregroundStyle(th.text).bold().lineLimit(1).layoutPriority(1)
            Text(now.artists).foregroundStyle(th.sub).lineLimit(1)
            Spacer(minLength: 8)
            icon("shuffle", on: now.shuffle, help: "Shuffle (⌃S)", model.toggleShuffle)
            icon(now.repeats == "one" ? "repeat.1" : "repeat", on: now.repeats != "off", help: "Repeat (⌃R)", model.cycleRepeat)
        }
        .padding(.horizontal, 12)
        .frame(height: nowHeight)
        .modifier(DragsWindow())
    }
}

extension NowPlaying {
    /// An SF Symbol button: the theme's accent when on, faint when off.
    func icon(_ name: String, on: Bool, help: String, _ action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Image(systemName: name)
                .font(.system(size: 11, weight: .semibold))
                .foregroundStyle(on ? th.accent : th.dim)
                .frame(width: 18, height: 18)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .help(help)
    }
}

/// What's playing after a media key: the bar's now playing line, on its own and hands off.
struct Toast: View {
    let now: Playing
    let th: Theme

    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: now.playing ? "play.fill" : "pause.fill")
                .font(.system(size: 11, weight: .semibold))
                .foregroundStyle(th.accent)
            // no priority: past toastWidth both get an even share, each ending in …
            Text(now.name).foregroundStyle(th.text).bold().lineLimit(1)
            Text(now.artists).foregroundStyle(th.sub).lineLimit(1)
        }
        .font(mono)
        .padding(.horizontal, 14)
        .frame(height: barHeight)
        .modifier(Glass(th: th))
        .padding(margin)
    }
}

/// `🔍 query▏          songs · albums · artists`
struct Prompt: View {
    @ObservedObject var model: Model
    var focused: FocusState<Bool>.Binding
    let th: Theme

    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: "magnifyingglass")
                .font(.system(size: 13, weight: .medium))
                .foregroundStyle(th.sub)
            // the placeholder is ours, not the field's: AppKit's keeps its first color and isn't vibrant on glass
            ZStack(alignment: .leading) {
                if model.query.isEmpty {
                    Text("Search \(model.kind.rawValue)").foregroundStyle(th.dim).allowsHitTesting(false)
                }
                TextField("", text: $model.query)
                    .textFieldStyle(.plain)
                    .foregroundStyle(th.text)
                    .focused(focused)
            }
            if model.busy {
                Text("…").foregroundStyle(th.dim)
            }
            HStack(spacing: 0) {
                ForEach(Array(Kind.allCases.enumerated()), id: \.element) { i, k in
                    if i > 0 { Text(" · ").foregroundStyle(th.dim) }
                    Text(k.rawValue).foregroundStyle(k == model.kind ? th.gold : th.dim)
                }
            }
        }
        .padding(.horizontal, 12)
        .frame(height: barHeight)
        .modifier(DragsWindow())
    }
}

/// A numbered row like spot's list: number, title (outlined E if explicit), details, time.
struct Row: View {
    let n: Int
    let result: Result
    let songs: Bool
    let selected: Bool
    let margin: (top: CGFloat, bottom: CGFloat)
    let th: Theme

    var body: some View {
        HStack(spacing: 14) {
            Text("\(n)").foregroundStyle(th.dim).frame(width: 20, alignment: .trailing)
            HStack(spacing: 6) {
                Text(result.name).foregroundStyle(th.text).lineLimit(1)
                if result.explicit {
                    Text("\u{1F134}").foregroundStyle(th.dim)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            Text(result.sub).foregroundStyle(th.sub).lineLimit(1)
                .frame(maxWidth: .infinity, alignment: .leading)
            if songs {
                Text(result.duration).foregroundStyle(th.dim).frame(width: 40, alignment: .trailing)
            }
        }
        .padding(.horizontal, 12)
        .frame(height: rowHeight)
        .padding(.top, margin.top)
        .padding(.bottom, margin.bottom)
        .background(selected ? th.hl : .clear)
    }
}

// MARK: window

/// A borderless panel that can still take keys.
final class Panel: NSPanel {
    override var canBecomeKey: Bool { true }
    override var canBecomeMain: Bool { true }
}

/// The toast's window: its transparent margin may sit up under the menu bar, or past the screen's
/// edge, which AppKit would otherwise push it down from.
final class ToastPanel: NSPanel {
    override func constrainFrameRect(_ frame: NSRect, to screen: NSScreen?) -> NSRect { frame }
}

@MainActor
final class Launcher: NSObject, NSApplicationDelegate, NSWindowDelegate {
    let model = Model()
    var panel: Panel!
    var hotKey: EventHotKeyRef?
    var changes: AnyCancellable?
    var poll: Timer?  // refreshes what's playing while the window is open
    var bottom: CGFloat = 0  // the glass's bottom edge (the window's is `margin` lower): it grows upward from here
    var placing = false  // moving the window ourselves, not the user dragging it
    var toast: ToastPanel!
    var toastView: NSHostingController<Toast?>!
    var toastFade: Task<Void, Never>?

    func applicationDidFinishLaunching(_ note: Notification) {
        panel = Panel(
            contentRect: NSRect(x: 0, y: 0, width: width + 2 * margin, height: barHeight + 2 * margin),
            styleMask: [.borderless, .nonactivatingPanel, .fullSizeContentView],
            backing: .buffered, defer: false)
        panel.isFloatingPanel = true
        panel.level = .modalPanel
        panel.collectionBehavior = [.moveToActiveSpace, .fullScreenAuxiliary, .transient]
        panel.backgroundColor = .clear
        panel.isOpaque = false
        panel.isMovableByWindowBackground = true
        panel.hasShadow = false  // the window server's is square at the corners; the glass draws its own
        panel.hidesOnDeactivate = false
        panel.delegate = self
        let host = NSHostingView(rootView: LauncherView(model: model))
        host.sizingOptions = []  // fit() alone sizes the window
        panel.contentView = host
        model.close = { [weak self] in self?.hide() }
        // resize after each change to what's shown (the model's change lands after willChange)
        changes = model.objectWillChange.sink { [weak self] _ in
            DispatchQueue.main.async { self?.fit() }
        }

        NSEvent.addLocalMonitorForEvents(matching: .keyDown) { [weak self] e in
            guard let self, self.panel.isKeyWindow else { return e }
            return self.key(e) ? nil : e
        }
        registerHotKey()
        makeToast()
        Thread.detachNewThread { [self] in
            watchSkips { Task { @MainActor in await self.skipped() } }
        }
        NSWorkspace.shared.notificationCenter.addObserver(
            self, selector: #selector(activeSpaceChanged),
            name: NSWorkspace.activeSpaceDidChangeNotification, object: nil)
        if SMAppService.mainApp.status == .notRegistered { try? SMAppService.mainApp.register() }
    }

    /// Arrows, return, tab and escape; everything else goes to the text field.
    func key(_ e: NSEvent) -> Bool {
        let cmd = e.modifierFlags.contains(.command)
        switch Int(e.keyCode) {
        case kVK_Escape: hide()
        case kVK_DownArrow: model.move(1)
        case kVK_UpArrow: model.move(-1)
        case kVK_ANSI_N where e.modifierFlags.contains(.control): model.move(1)
        case kVK_ANSI_P where e.modifierFlags.contains(.control): model.move(-1)
        case kVK_Return, kVK_ANSI_KeypadEnter: model.choose(queue: cmd)
        case kVK_Tab: model.cycleKind(back: e.modifierFlags.contains(.shift))
        case kVK_ANSI_S where e.modifierFlags.contains(.control): model.toggleShuffle()
        case kVK_ANSI_R where e.modifierFlags.contains(.control): model.cycleRepeat()
        case kVK_ANSI_Q where cmd: NSApp.terminate(nil)
        case kVK_ANSI_C where cmd: model.copyLink()
        default: return false
        }
        return true
    }

    func registerHotKey() {
        var spec = EventTypeSpec(eventClass: OSType(kEventClassKeyboard), eventKind: UInt32(kEventHotKeyPressed))
        InstallEventHandler(GetApplicationEventTarget(), { _, _, ctx in
            let me = Unmanaged<Launcher>.fromOpaque(ctx!).takeUnretainedValue()
            MainActor.assumeIsolated { me.toggle() }
            return noErr
        }, 1, &spec, Unmanaged.passUnretained(self).toOpaque(), nil)
        let id = EventHotKeyID(signature: OSType(0x5350_4F54), id: 1)  // 'SPOT'
        let status = RegisterEventHotKey(UInt32(kVK_Space), UInt32(optionKey), id, GetApplicationEventTarget(), 0, &hotKey)
        if status != noErr { NSLog("spot launcher: couldn't register ⌥Space (\(status)); is something else using it?") }
    }

    func toggle() {
        panel.isVisible ? hide() : show()
    }

    func show() {
        toastFade?.cancel()
        toast.orderOut(nil)
        model.reset()
        let midX: CGFloat
        (midX, bottom) = home()
        place(NSRect(x: midX - width / 2, y: bottom, width: width, height: barHeight).insetBy(dx: -margin, dy: -margin))
        // keys without activating, like Spotlight: the app underneath stays frontmost, so closing has no focus to
        // hand back (handing it back is what dragged a desktop swipe back to the old desktop)
        panel.makeKeyAndOrderFront(nil)
        model.refreshNow()
        poll = Timer.scheduledTimer(withTimeInterval: 2, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { self?.model.refreshNow() }
        }
    }

    /// Where the bar opens: where it was dragged to last (if that's still on a screen), else
    /// bottom center, clear of the Dock. Its horizontal middle and its bottom edge.
    func home() -> (CGFloat, CGFloat) {
        let d = UserDefaults.standard
        let (midX, bottom) = (d.double(forKey: "midX"), d.double(forKey: "bottom"))
        let saved = NSPoint(x: midX, y: bottom + barHeight / 2)
        if d.object(forKey: "midX") != nil && NSScreen.screens.contains(where: { NSMouseInRect(saved, $0.visibleFrame, false) }) {
            return (midX, bottom)
        }
        let screen = NSScreen.screens.first { NSMouseInRect(NSEvent.mouseLocation, $0.frame, false) } ?? NSScreen.main!
        let f = screen.visibleFrame
        return (f.midX, f.minY + f.height * 0.12)
    }

    func makeToast() {
        toast = ToastPanel(
            contentRect: NSRect(x: 0, y: 0, width: width, height: barHeight),
            styleMask: [.borderless, .nonactivatingPanel, .fullSizeContentView],
            backing: .buffered, defer: false)
        toast.isFloatingPanel = true
        toast.level = .modalPanel
        toast.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary, .stationary, .ignoresCycle]
        toast.backgroundColor = .clear
        toast.isOpaque = false
        toast.hasShadow = false
        toast.hidesOnDeactivate = false
        toast.ignoresMouseEvents = true
        toastView = NSHostingController(rootView: nil)
        toastView.sizingOptions = []
        toast.contentView = toastView.view
    }

    /// A media key was pressed. The bar already shows what's playing; otherwise, a moment's toast.
    func skipped() async {
        await model.fetchNow()
        await model.fetchPopup()
        guard !panel.isVisible, model.popup, let now = model.now else { return }
        toastView.rootView = Toast(now: now, th: model.theme)
        let size = toastView.sizeThatFits(in: NSSize(width: toastWidth + 2 * margin, height: barHeight + 2 * margin))
        // on the screen you're on: right under the notch, or the menu bar on a screen without one
        let screen = NSScreen.screens.first { NSMouseInRect(NSEvent.mouseLocation, $0.frame, false) } ?? NSScreen.main!
        let top = screen.safeAreaInsets.top > 0 ? screen.frame.maxY - screen.safeAreaInsets.top : screen.visibleFrame.maxY
        toast.setFrame(NSRect(x: screen.frame.midX - size.width / 2, y: top - toastGap + margin - size.height, width: size.width, height: size.height), display: true)
        toast.alphaValue = 1
        toast.orderFrontRegardless()
        toastFade?.cancel()
        toastFade = Task {
            try? await Task.sleep(for: .seconds(model.popupSeconds))
            guard !Task.isCancelled else { return }
            await NSAnimationContext.runAnimationGroup { $0.duration = 0.25; toast.animator().alphaValue = 0 }
            if !Task.isCancelled { toast.orderOut(nil) }
        }
    }

    func hide() {
        guard panel.isVisible else { return }
        poll?.invalidate()
        poll = nil
        panel.orderOut(nil)
    }

    @objc func activeSpaceChanged(_ note: Notification) { hide() }

    /// The prompt line and what's playing under it, plus the results (or a note) and a rule above it. The bottom edge stays put,
    /// so it grows upward.
    func fit() {
        let above: CGFloat = model.note != nil ? barHeight + 1 : model.results.isEmpty ? 0 : CGFloat(model.results.count) * rowHeight + 12 + 1
        let h = barHeight + above + (model.now != nil ? nowHeight : 0)
        guard abs(panel.frame.height - h - 2 * margin) > 0.5 || abs(panel.frame.minY + margin - bottom) > 0.5 else { return }
        place(NSRect(x: panel.frame.midX - width / 2, y: bottom, width: width, height: h).insetBy(dx: -margin, dy: -margin))
    }

    func place(_ frame: NSRect) {
        placing = true
        panel.setFrame(frame, display: true)
        placing = false
    }

    /// The user dragged it: grow from there, and open there next time.
    func windowDidMove(_ n: Notification) {
        guard !placing, panel.isVisible else { return }
        bottom = panel.frame.minY + margin
        UserDefaults.standard.set(panel.frame.midX, forKey: "midX")
        UserDefaults.standard.set(bottom, forKey: "bottom")
    }

    func windowDidResignKey(_ n: Notification) { hide() }
}

let app = NSApplication.shared
let launcher = MainActor.assumeIsolated { Launcher() }
app.delegate = launcher
app.setActivationPolicy(.accessory)
app.run()
