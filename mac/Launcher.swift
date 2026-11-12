// Spot Launcher: ⌥Space from any app opens a Spotlight-style bar on Liquid Glass that searches
// Spotify through the running spot and plays what you pick. Empty, it lists what was played
// lately; typed in, the best match among your playlists comes first, then Spotify's top result,
// songs, an album and an artist. ↑↓ move, tab jumps to the next section, → opens an album,
// playlist or artist's songs (← or esc goes back), ⏎ plays, ⌘⏎ queues, ⌘C copies a link (with
// no results, to what's playing), ⌃S shuffle, ⌃R repeat, esc closes.
//
// Plain AppKit, no SwiftUI: one text field and one view that draws the rest, in the system's
// label colors with vibrancy, the way Apple's own apps sit on glass (covers in a view above it). It asks the player
// over its socket (~/.cache/spot/sock) with `launcher …` requests (src/launch.rs).

import AppKit
import Carbon.HIToolbox
import ServiceManagement

// MARK: talking to spot

let sockPath = NSHomeDirectory() + "/.cache/spot/sock"

func connectSpot() -> Int32? {
    let fd = socket(AF_UNIX, SOCK_STREAM, 0)
    guard fd >= 0 else { return nil }
    var addr = sockaddr_un()
    addr.sun_family = sa_family_t(AF_UNIX)
    withUnsafeMutableBytes(of: &addr.sun_path) { buf in
        sockPath.utf8CString.withUnsafeBytes { buf.copyMemory(from: UnsafeRawBufferPointer(rebasing: $0.prefix(buf.count - 1))) }
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
/// Blocks: call it off the main thread.
func spot(_ args: [String]) -> (Int, String) {
    guard let fd = connectSpot() else { return (3, "spot isn't running") }
    defer { close(fd) }
    let line = Array((args.joined(separator: "\t") + "\n").utf8)
    guard write(fd, line, line.count) == line.count else { return (1, "couldn't ask spot") }
    var data = Data()
    var buf = [UInt8](repeating: 0, count: 65536)
    while true {
        let n = read(fd, &buf, buf.count)
        if n <= 0 { break }
        data.append(buf, count: n)
    }
    let reply = String(decoding: data, as: UTF8.self)
    let (head, rest) = reply.firstIndex(of: "\n").map { (String(reply[..<$0]), String(reply[reply.index(after: $0)...])) } ?? (reply, "")
    guard let code = Int(head) else { return (1, "no answer from spot (an older one? `spot quit`, then start it again)") }
    return (code, rest.trimmingCharacters(in: .newlines))
}

/// Ask spot off the main thread; `done` gets the answer on it.
func ask(_ args: [String], _ done: @escaping @MainActor (Int, String) -> Void = { _, _ in }) {
    DispatchQueue.global(qos: .userInitiated).async {
        let r = spot(args)
        DispatchQueue.main.async { MainActor.assumeIsolated { done(r.0, r.1) } }
    }
}

/// Follow `spot watch` for good, calling `changed` per line (a new song, paused, modes…) and
/// when the player comes or goes. While spot isn't running it looks again every few seconds.
func watchPlayer(_ changed: @escaping @Sendable () -> Void) {
    while true {
        if let fd = connectSpot() {
            var buf = [UInt8](repeating: 0, count: 4096)
            if write(fd, "watch\n", 6) == 6 {
                changed()
                while read(fd, &buf, buf.count) > 0 { changed() }
            }
            close(fd)
            changed()
        }
        sleep(3)
    }
}

/// `spot start`, when the bar opens with no player: looks where install.sh and cargo put it.
func startSpot() {
    let home = NSHomeDirectory()
    let candidates = ["\(home)/.local/bin/spot", "\(home)/.cargo/bin/spot", "/opt/homebrew/bin/spot", "/usr/local/bin/spot"]
    guard let exe = candidates.first(where: { FileManager.default.isExecutableFile(atPath: $0) }) else { return }
    let p = Process()
    p.executableURL = URL(fileURLWithPath: exe)
    p.arguments = ["start"]
    p.standardOutput = FileHandle.nullDevice
    p.standardError = FileHandle.nullDevice
    try? p.run()
    p.waitUntilExit()
}

// MARK: what it lists

struct Hit {
    let uri, name, sub, cover: String
    let ms: Int
    let explicit: Bool
    let section: String
    let number: Int  // inside an album: the song's track number

    /// A row of `launcher search / songs / lists`: uri, name, subtitle, cover, ms, explicit,
    /// section, track number.
    init?(_ line: Substring) {
        let f = line.split(separator: "\t", omittingEmptySubsequences: false).map(String.init)
        guard f.count >= 7 else { return nil }
        (uri, name, sub, cover) = (f[0], f[1], f[2], f[3])
        ms = Int(f[4]) ?? 0
        explicit = f[5] == "1"
        section = f[6]
        number = f.count > 7 ? Int(f[7]) ?? 0 : 0
    }

    /// An album, playlist or artist: → lists its songs.
    var opens: Bool { uri == "liked" || ["spotify:album:", "spotify:playlist:", "spotify:artist:"].contains { uri.hasPrefix($0) } }
    var round: Bool { uri.hasPrefix("spotify:artist:") }
}

func rows(_ text: String) -> [Hit] { text.split(separator: "\n").compactMap(Hit.init) }

struct Now {
    var playing: Bool
    let name, artists, uri, cover: String
    var pos, len: Int  // ms
    var shuffle: Bool
    var repeatMode: String
    let at = Date()  // when pos was true

    init?(_ line: String) {
        let f = line.split(separator: "\t", omittingEmptySubsequences: false).map(String.init)
        guard f.count >= 9 else { return nil }
        playing = f[0] == "1"
        (name, artists, uri, cover) = (f[1], f[2], f[3], f[4])
        (pos, len) = (Int(f[5]) ?? 0, Int(f[6]) ?? 0)
        shuffle = f[7] == "1"
        repeatMode = f[8]
    }

    var ratio: CGFloat {
        guard len > 0 else { return 0 }
        let p = Double(pos) + (playing ? Date().timeIntervalSince(at) * 1000 : 0)
        return CGFloat(min(p / Double(len), 1))
    }
}

/// How well `q` names `name`: 3 exactly, 2 its start, 1 inside it, 0 not at all.
func matchScore(_ q: String, _ name: String) -> Int {
    let (q, n) = (q.lowercased(), name.lowercased())
    if n == q { return 3 }
    if n.hasPrefix(q) { return 2 }
    return n.contains(q) ? 1 : 0
}

/// spotify:track:id as https://open.spotify.com/track/id
func shareLink(_ uri: String) -> String? {
    guard uri.hasPrefix("spotify:") else { return nil }
    return "https://open.spotify.com/" + uri.dropFirst("spotify:".count).replacingOccurrences(of: ":", with: "/")
}

/// Spotify's 64 px copy of a cover (its urls name the size).
func small(_ url: String) -> String {
    url.replacingOccurrences(of: "ab67616d0000b273", with: "ab67616d00004851")
        .replacingOccurrences(of: "ab67616d00001e02", with: "ab67616d00004851")
        .replacingOccurrences(of: "ab6761610000e5eb", with: "ab6761610000f178")
        .replacingOccurrences(of: "ab67616100005174", with: "ab6761610000f178")
}

func mmss(_ ms: Int) -> String { String(format: "%d:%02d", ms / 60000, ms / 1000 % 60) }

// MARK: sizes and colors

let width: CGFloat = 640
let margin: CGFloat = 24  // around the card, for its shadow
let barH: CGFloat = 54
let hitH: CGFloat = 38
let footH: CGFloat = 50
let headH: CGFloat = 22
let thumb: CGFloat = 28
let height: CGFloat = 492  // the window's, always: it never grows into the bottom of the screen
let listTop = barH + 1 + 4
let room = height - 2 * margin - barH - 12 - footH  // what the list has room for
let recentLists = 3  // on the empty bar: lists you started,
let recentSongs = 6  // songs you heard (more scroll),
let fillTo = 7       // and your playlists after them, up to this many rows
let corner: CGFloat = 24

// MARK: the window

/// A borderless panel that still takes keys.
final class Panel: NSPanel {
    override var canBecomeKey: Bool { true }
}

/// The field: the bar's words. Arrow keys and the like go to the launcher first.
final class Field: NSTextField {
    override var acceptsFirstResponder: Bool { true }
}

@MainActor
final class Launcher: NSObject, NSApplicationDelegate, NSWindowDelegate, NSTextFieldDelegate {
    var panel: Panel!
    var card: Card!
    var field: Field!
    var hotKey: EventHotKeyRef?
    var tick: Timer?

    // what's listed
    var results: [Hit] = []
    var picked = 0
    var scroll = 0
    var recent: [Hit] = []
    var playlists: [Hit] = []
    var inside: (parent: Hit, back: [Hit], picked: Int, scroll: Int)?
    var note: String?
    var busy = false
    var generation = 0
    var now: Now?
    var copied = false
    var hover: Int?
    let images = NSCache<NSString, NSImage>()
    var loading = Set<String>()

    var query: String { field.stringValue.trimmingCharacters(in: .whitespaces) }

    func applicationDidFinishLaunching(_ n: Notification) {
        panel = Panel(contentRect: NSRect(x: 0, y: 0, width: width + 2 * margin, height: height),
                      styleMask: [.borderless, .nonactivatingPanel, .fullSizeContentView], backing: .buffered, defer: false)
        panel.isFloatingPanel = true
        panel.level = .modalPanel
        panel.collectionBehavior = [.moveToActiveSpace, .fullScreenAuxiliary, .transient]
        panel.backgroundColor = .clear
        panel.isOpaque = false
        panel.hasShadow = false  // the window server's is square; the card casts its own
        panel.hidesOnDeactivate = false
        panel.delegate = self
        panel.animationBehavior = .none

        let root = NSView(frame: NSRect(x: 0, y: 0, width: width + 2 * margin, height: height))
        let frame = root.bounds.insetBy(dx: margin, dy: margin)
        // the shadow, under the glass (the glass view clips what it holds)
        let shadow = NSView(frame: frame)
        shadow.wantsLayer = true
        shadow.layer?.shadowPath = CGPath(roundedRect: shadow.bounds, cornerWidth: corner, cornerHeight: corner, transform: nil)
        if #available(macOS 26, *) {
            // glass floats: a soft shadow, its own rim does the rest
            shadow.layer?.shadowOpacity = 0.3
            shadow.layer?.shadowRadius = 22
            shadow.layer?.shadowOffset = CGSize(width: 0, height: -10)
        } else {
            shadow.layer?.shadowOpacity = 1
            shadow.layer?.shadowRadius = 24
            shadow.layer?.shadowOffset = CGSize(width: 0, height: -18)
        }
        root.addSubview(shadow)

        card = Card(frame: NSRect(origin: .zero, size: frame.size))
        card.wantsLayer = true
        card.launcher = self
        field = Field(frame: NSRect(x: 54, y: (barH - 28) / 2, width: frame.width - 54 - 20, height: 28))
        field.isBordered = false
        field.drawsBackground = false
        field.focusRingType = .none
        field.font = .systemFont(ofSize: 20, weight: .regular)
        field.cell?.isScrollable = true
        field.cell?.wraps = false
        field.delegate = self
        card.addSubview(field)

        let glass: NSView
        if #available(macOS 26, *) {
            let g = NSGlassEffectView(frame: frame)
            // the system's glass, untinted: the wallpaper shows through, blurred enough for the
            // vibrant label colors to read (clear glass barely blurs; windows behind stay sharp)
            g.style = .regular
            g.cornerRadius = corner
            g.contentView = card
            glass = g
        } else {
            let v = NSVisualEffectView(frame: frame)
            v.material = .hudWindow
            v.blendingMode = .behindWindow
            v.state = .active
            v.wantsLayer = true
            v.layer?.cornerRadius = corner
            v.layer?.masksToBounds = true
            v.addSubview(card)
            glass = v
        }
        root.addSubview(glass)
        panel.contentView = root
        applyColors()

        NSEvent.addLocalMonitorForEvents(matching: .keyDown) { [weak self] e in
            guard let self, self.panel.isKeyWindow else { return e }
            return self.key(e) ? nil : e
        }
        registerHotKey()
        NSWorkspace.shared.notificationCenter.addObserver(self, selector: #selector(spaceChanged), name: NSWorkspace.activeSpaceDidChangeNotification, object: nil)
        DistributedNotificationCenter.default().addObserver(self, selector: #selector(appearanceChanged), name: Notification.Name("AppleInterfaceThemeChangedNotification"), object: nil)
        NotificationCenter.default.addObserver(self, selector: #selector(appearanceChanged), name: NSColor.systemColorsDidChangeNotification, object: nil)  // the accent color
        Thread.detachNewThread {
            watchPlayer {
                DispatchQueue.main.async {
                    MainActor.assumeIsolated {
                        guard let me = NSApp.delegate as? Launcher, me.panel.isVisible else { return }
                        me.fetchNow()
                    }
                }
            }
        }
        // a login item once installed (a test build elsewhere isn't)
        if Bundle.main.bundlePath.contains("/Applications/"), SMAppService.mainApp.status == .notRegistered { try? SMAppService.mainApp.register() }
        if CommandLine.arguments.contains("--show") { show() }
    }

    /// Opening the app again (`open -a "Spot Launcher"`) opens the bar, as the hotkey does.
    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool {
        show()
        return false
    }

    var dark: Bool { panel.effectiveAppearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua }

    func applyColors() {
        field.textColor = .labelColor
        field.placeholderAttributedString = NSAttributedString(string: "Songs, albums, artists, your playlists",
            attributes: [.foregroundColor: NSColor.placeholderTextColor, .font: NSFont.systemFont(ofSize: 20)])
        (field.currentEditor() as? NSTextView)?.insertionPointColor = .labelColor
        card.needsDisplay = true
        card.updateProgress()
    }

    @objc func appearanceChanged() { DispatchQueue.main.async { self.applyColors() } }

    // MARK: hotkey

    func registerHotKey() {
        var spec = EventTypeSpec(eventClass: OSType(kEventClassKeyboard), eventKind: UInt32(kEventHotKeyPressed))
        InstallEventHandler(GetApplicationEventTarget(), { _, _, ctx in
            let me = Unmanaged<Launcher>.fromOpaque(ctx!).takeUnretainedValue()
            MainActor.assumeIsolated { me.panel.isVisible ? me.hide() : me.show() }
            return noErr
        }, 1, &spec, Unmanaged.passUnretained(self).toOpaque(), nil)
        // `defaults write com.github.boopdotpng.spot.launcher hotkey cmd+shift+space` to change it
        let wanted = UserDefaults.standard.string(forKey: "hotkey") ?? "option+space"
        guard let (code, mods) = parseHotKey(wanted) ?? parseHotKey("option+space") else { return }
        let id = EventHotKeyID(signature: OSType(0x5350_4F54), id: 1)  // 'SPOT'
        let status = RegisterEventHotKey(code, mods, id, GetApplicationEventTarget(), 0, &hotKey)
        if status != noErr { NSLog("spot launcher: couldn't register \(wanted) (\(status)); is something else using it?") }
    }

    // MARK: open and close

    func show() {
        reset()
        let (midX, bottom) = home()
        panel.setFrameOrigin(NSPoint(x: midX - panel.frame.width / 2, y: bottom - margin))
        // keys without activating, like Spotlight: the app underneath stays frontmost
        panel.makeKeyAndOrderFront(nil)
        panel.makeFirstResponder(field)
        applyColors()
        fetchNow()
        ask(["launcher", "lists"]) { code, text in
            if code == 3 {
                self.note = "starting spot…"
                self.card.needsDisplay = true
                DispatchQueue.global().async {
                    startSpot()
                    DispatchQueue.main.async { MainActor.assumeIsolated { self.note = nil; self.fetchLists(); self.fetchNow() } }
                }
                return
            }
            self.gotLists(code, text)
        }
        // the progress bar is a view of its own, moved once a second: redrawing the card that
        // often kept ~100 MB of the window server's buffers in use
        tick = Timer.scheduledTimer(withTimeInterval: 1, repeats: true) { _ in
            MainActor.assumeIsolated { (NSApp.delegate as? Launcher)?.card.updateProgress() }
        }
    }

    func hide() {
        guard panel.isVisible else { return }
        tick?.invalidate()
        panel.orderOut(nil)
        reset()
    }

    func reset() {
        generation += 1  // answers on their way are moot
        field.stringValue = ""
        results = recent
        inside = nil
        note = nil
        busy = false
        picked = 0
        scroll = 0
        copied = false
        hover = nil
        card.needsDisplay = true
    }

    /// Where it opens: where it was dragged to last, if that's still on a screen, else low and
    /// centered on the screen with the pointer. Its middle and its card's bottom edge.
    func home() -> (CGFloat, CGFloat) {
        let d = UserDefaults.standard
        if d.object(forKey: "midX") != nil {
            let (x, y) = (d.double(forKey: "midX"), d.double(forKey: "bottom"))
            if NSScreen.screens.contains(where: { NSMouseInRect(NSPoint(x: x, y: y + 20), $0.visibleFrame, false) }) { return (x, y) }
        }
        let screen = NSScreen.screens.first { NSMouseInRect(NSEvent.mouseLocation, $0.frame, false) } ?? NSScreen.main!
        let f = screen.visibleFrame
        return (f.midX, max(f.minY + 40, f.midY - (height - 2 * margin) / 2 - 60))
    }

    func windowDidMove(_ n: Notification) {
        guard panel.isVisible else { return }
        UserDefaults.standard.set(panel.frame.midX, forKey: "midX")
        UserDefaults.standard.set(panel.frame.minY + margin, forKey: "bottom")
    }

    func windowDidResignKey(_ n: Notification) { hide() }
    @objc func spaceChanged() { hide() }

    // MARK: the player

    func fetchNow() {
        ask(["launcher", "status"]) { code, text in
            self.now = code == 0 ? Now(text) : nil
            self.card.needsDisplay = true
            self.card.updateProgress()
        }
    }

    func fetchLists() { ask(["launcher", "lists"]) { self.gotLists($0, $1) } }

    func gotLists(_ code: Int, _ text: String) {
        guard code == 0 else { return }
        let all = rows(text)
        playlists = all.filter { $0.section == "Your playlists" }
        // the lists you started and the songs you heard lately; short of a full bar, your
        // playlists after them, so it never opens bare
        let lists = all.filter { $0.section == "Recently played" }.prefix(recentLists)
        let songs = all.filter { $0.section == "Recent songs" }.prefix(recentSongs)
        recent = Array(lists) + Array(songs)
        let more = playlists.filter { p in !lists.contains { $0.uri == p.uri } }
        recent += more.prefix(max(0, fillTo - recent.count))
        if query.isEmpty && inside == nil {
            results = recent
            picked = min(picked, max(results.count - 1, 0))
            card.needsDisplay = true
        }
    }

    // MARK: searching

    func controlTextDidChange(_ n: Notification) { search() }

    /// Search as you type: waits a moment for typing to stop and drops answers to older words.
    /// Your playlist that matches shows at once; Spotify's results after.
    func search() {
        generation += 1
        let g = generation, q = query
        inside = nil
        note = nil
        picked = 0
        scroll = 0
        if q.isEmpty {
            results = recent
            busy = false
            card.needsDisplay = true
            return
        }
        results = matching(q)
        busy = true
        card.needsDisplay = true
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.14) {
            guard self.generation == g else { return }
            ask(["launcher", "search", q]) { code, text in
                guard self.generation == g else { return }
                self.busy = false
                let keep = self.results.indices.contains(self.picked) ? self.results[self.picked].uri : nil
                let remote = code == 0 ? rows(text) : []
                self.note = code == 0 ? nil : text
                self.results = self.matching(q) + remote
                if self.results.isEmpty && self.note == nil { self.note = "no results" }
                self.scroll = 0
                self.pick(keep.flatMap { u in self.results.firstIndex { $0.uri == u } } ?? 0)
            }
        }
    }

    /// The one of your playlists the words name best.
    func matching(_ q: String) -> [Hit] {
        let scored = playlists.map { (matchScore(q, $0.name), $0) }.filter { $0.0 > 0 }
        return scored.max { $0.0 < $1.0 }.map { [$0.1] } ?? []
    }

    // MARK: moving about

    /// The rows on screen from row `from`, as many as fit: each one's index, and whether it
    /// starts a section (has a title).
    func shown(from: Int? = nil) -> [(Int, Bool)] {
        let from = from ?? scroll
        var used: CGFloat = inside != nil ? headH : 0
        var out: [(Int, Bool)] = []
        for i in results.indices where i >= from {
            let s = results[i].section
            let titled = !s.isEmpty && (i == from || results[i - 1].section != s)
            used += hitH + (titled ? headH : 0)
            if used > room + 0.5 && !out.isEmpty { break }
            out.append((i, titled))
        }
        return out
    }

    func pick(_ i: Int) {
        picked = i
        if i < scroll { scroll = i }
        while scroll < i, let last = shown().last?.0, last < i { scroll += 1 }
        card.needsDisplay = true
    }

    /// tab: the first row of the next section (⇧: of this one, or the one before).
    func nextSection(back: Bool) {
        let starts = results.indices.filter { $0 == 0 || results[$0 - 1].section != results[$0].section }
        guard !starts.isEmpty else { return }
        pick(back ? starts.last { $0 < picked } ?? starts.last! : starts.first { $0 > picked } ?? 0)
    }

    /// →: the picked album, playlist or artist's songs, in place.
    func openPicked() -> Bool {
        guard inside == nil, results.indices.contains(picked), results[picked].opens else { return false }
        let h = results[picked]
        generation += 1
        let g = generation
        inside = (h, results, picked, scroll)
        results = []
        (picked, scroll, note, busy) = (0, 0, nil, true)
        card.needsDisplay = true
        ask(["launcher", "songs", h.uri]) { code, text in
            guard self.generation == g, self.inside != nil else { return }
            self.busy = false
            if code == 0 {
                self.results = rows(text)
                self.note = self.results.isEmpty ? "no songs" : nil
            } else {
                self.note = text
            }
            self.card.needsDisplay = true
        }
        return true
    }

    /// ← or esc inside: back to what it was opened from.
    func goBack() -> Bool {
        guard let i = inside else { return false }
        generation += 1
        inside = nil
        (results, picked, scroll, note, busy) = (i.back, i.picked, i.scroll, nil, false)
        card.needsDisplay = true
        return true
    }

    // MARK: doing

    /// Play (or queue) the picked result, then get out of the way. Typed faster than the results
    /// came: the bar closes at once and the first result plays once it's found.
    func choose(queue: Bool) {
        let local = results.indices.contains(picked) && results[picked].section == "Your playlists"
        if inside == nil && ((busy && !local) || results.isEmpty) {
            let q = query
            guard !q.isEmpty else { return }
            hide()
            ask(["launcher", "search", q]) { code, text in
                guard code == 0, let first = rows(text).first else { return }
                ask(queue ? ["launcher", "queue", first.uri] : ["play", first.uri])
            }
            return
        }
        guard results.indices.contains(picked) else { return }
        let uri = results[picked].uri
        // a song inside an album or playlist plays it from there on; an artist's, on its own
        let from = inside.map(\.parent.uri).flatMap { $0.hasPrefix("spotify:artist:") || queue ? nil : $0 }
        let args = queue ? ["launcher", "queue", uri] : from.map { ["play", $0, "--from", uri] } ?? ["play", uri]
        ask(args) { code, text in
            if code == 0 {
                self.hide()
            } else {
                self.note = text.split(separator: "\n").first.map(String.init)
                self.card.needsDisplay = true
            }
        }
    }

    /// ⌘C: the link to the picked result, or with no results what's playing.
    func copyLink() {
        let uri = results.indices.contains(picked) ? results[picked].uri : now?.uri
        guard let link = uri.flatMap(shareLink) else { return }
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(link, forType: .string)
        copied = true
        card.needsDisplay = true
        DispatchQueue.main.asyncAfter(deadline: .now() + 1.2) {
            self.copied = false
            self.card.needsDisplay = true
        }
    }

    func toggleShuffle() {
        guard var n = now else { return }
        n.shuffle.toggle()
        now = n
        card.needsDisplay = true
        ask(["set", "shuffle=\(n.shuffle ? "on" : "off")"])
    }

    func cycleRepeat() {
        guard var n = now else { return }
        n.repeatMode = ["off": "all", "all": "one"][n.repeatMode] ?? "off"
        now = n
        card.needsDisplay = true
        ask(["set", "repeat=\(n.repeatMode)"])
    }

    func togglePlay() {
        guard now != nil else { return }
        let paused = now!.playing
        ask(["set", "paused=\(paused ? "yes" : "no")"]) { _, _ in self.fetchNow() }
    }

    // MARK: keys

    /// What the field doesn't type: arrows, return, tab, escape, and the control keys.
    func key(_ e: NSEvent) -> Bool {
        let m = e.modifierFlags.intersection(.deviceIndependentFlagsMask)
        let (cmd, ctrl) = (m.contains(.command), m.contains(.control))
        let editor = field.currentEditor() as? NSTextView
        let caretAtEnd = editor.map { $0.selectedRange.length == 0 && $0.selectedRange.location == ($0.string as NSString).length } ?? true
        let n = results.count
        switch Int(e.keyCode) {
        case kVK_Escape: if !goBack() { hide() }
        case kVK_DownArrow where n > 0: pick((picked + 1) % n)
        case kVK_UpArrow where n > 0: pick((picked + n - 1) % n)
        case kVK_ANSI_N where ctrl && n > 0: pick((picked + 1) % n)
        case kVK_ANSI_P where ctrl && n > 0: pick((picked + n - 1) % n)
        case kVK_Return, kVK_ANSI_KeypadEnter: choose(queue: cmd)
        case kVK_Tab: nextSection(back: m.contains(.shift))
        case kVK_RightArrow where caretAtEnd && !m.contains(.shift): return openPicked()
        case kVK_LeftArrow where inside != nil: return goBack()
        case kVK_ANSI_S where ctrl: toggleShuffle()
        case kVK_ANSI_R where ctrl: cycleRepeat()
        case kVK_ANSI_C where cmd && (editor?.selectedRange.length ?? 0) == 0: copyLink()
        case kVK_ANSI_Q where cmd: NSApp.terminate(nil)
        default: return false
        }
        return true
    }

    // MARK: covers

    func image(_ url: String) -> NSImage? {
        guard !url.isEmpty else { return nil }
        let u = small(url)
        if let i = images.object(forKey: u as NSString) { return i }
        if !loading.contains(u), let url = URL(string: u) {
            loading.insert(u)
            URLSession.shared.dataTask(with: url) { data, _, _ in
                // decoded once, at the size it's drawn (2x), so the cache holds small bitmaps
                let img = data.flatMap(NSImage.init(data:)).map { downsized($0, to: thumb * 2) }
                DispatchQueue.main.async {
                    MainActor.assumeIsolated {
                        guard let me = NSApp.delegate as? Launcher else { return }
                        me.loading.remove(u)
                        if let img { me.images.setObject(img, forKey: u as NSString) }
                        me.card.needsDisplay = true
                    }
                }
            }.resume()
        }
        return nil
    }
}

func downsized(_ img: NSImage, to px: CGFloat) -> NSImage {
    guard let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: Int(px), pixelsHigh: Int(px), bitsPerSample: 8, samplesPerPixel: 4,
                                     hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0) else { return img }
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
    img.draw(in: NSRect(x: 0, y: 0, width: px, height: px))
    NSGraphicsContext.restoreGraphicsState()
    let out = NSImage(size: NSSize(width: px / 2, height: px / 2))
    out.addRepresentation(rep)
    return out
}

// MARK: drawing

/// Everything in the card but the field: the results, a note, what's playing at the foot. Its
/// words and shapes are vibrant (blended into the glass); covers and the accent go in `art`.
final class Card: NSView {
    weak var launcher: Launcher!
    override var isFlipped: Bool { true }
    override var mouseDownCanMoveWindow: Bool { false }
    override var allowsVibrancy: Bool { true }
    // (on a layer-backed view needsDisplay reads back false, so no `if needsDisplay` here)
    override var needsDisplay: Bool { didSet { art?.needsDisplay = true } }

    let progress = NSView()
    var art: Art!

    override init(frame: NSRect) {
        super.init(frame: frame)
        art = Art(frame: bounds)
        art.card = self
        art.wantsLayer = true
        art.autoresizingMask = [.width, .height]
        addSubview(art)
        progress.wantsLayer = true
        art.addSubview(progress)
    }
    required init?(coder: NSCoder) { fatalError() }

    /// How far through the song it is, along the foot's bottom edge.
    func updateProgress() {
        guard let l = launcher else { return }
        let f = footRect
        art.effectiveAppearance.performAsCurrentDrawingAppearance { progress.layer?.backgroundColor = NSColor.controlAccentColor.cgColor }
        progress.isHidden = l.now == nil
        progress.frame = NSRect(x: 0, y: f.maxY - 2, width: (f.width * (l.now?.ratio ?? 0)).rounded(), height: 2)
    }

    var footRect: NSRect { NSRect(x: 0, y: bounds.height - footH, width: bounds.width, height: footH) }
    var buttons: [(NSRect, String)] {
        let f = footRect
        return ["repeat", "play", "shuffle"].enumerated().map { i, k in (NSRect(x: f.maxX - 20 - 30 * CGFloat(i + 1) - 4 * CGFloat(i), y: f.midY - 15, width: 30, height: 30), k) }
    }

    /// Where each row on screen is drawn.
    func rowRects() -> [(Int, Bool, NSRect)] {
        guard let l = launcher else { return [] }
        var y = listTop + (l.inside != nil ? headH : 0)
        return l.shown().map { i, titled in
            if titled { y += headH }
            defer { y += hitH }
            return (i, titled, NSRect(x: 8, y: y, width: bounds.width - 16, height: hitH))
        }
    }

    override func updateTrackingAreas() {
        trackingAreas.forEach(removeTrackingArea)
        addTrackingArea(NSTrackingArea(rect: bounds, options: [.mouseMoved, .mouseEnteredAndExited, .activeAlways, .inVisibleRect], owner: self))
    }

    override func mouseMoved(with e: NSEvent) {
        let p = convert(e.locationInWindow, from: nil)
        let h = rowRects().first { NSMouseInRect(p, $0.2, true) }?.0
        if h != launcher.hover {
            launcher.hover = h
            needsDisplay = true
        }
    }

    override func mouseExited(with e: NSEvent) {
        launcher.hover = nil
        needsDisplay = true
    }

    override func mouseDown(with e: NSEvent) {
        let p = convert(e.locationInWindow, from: nil)
        if let (i, _, _) = rowRects().first(where: { NSMouseInRect(p, $0.2, true) }) {
            launcher.picked = i
            launcher.choose(queue: e.modifierFlags.contains(.command))
        } else if let (_, k) = buttons.first(where: { NSMouseInRect(p, $0.0, true) }), launcher.now != nil {
            switch k {
            case "shuffle": launcher.toggleShuffle()
            case "repeat": launcher.cycleRepeat()
            default: launcher.togglePlay()
            }
        } else if launcher.inside != nil && p.y >= listTop && p.y < listTop + headH {
            _ = launcher.goBack()
        } else {
            window?.performDrag(with: e)  // anywhere else moves it; it opens there next time
        }
    }

    override func draw(_ dirty: NSRect) {
        guard let l = launcher else { return }
        let text = NSColor.labelColor

        // the bar's magnifier, lit while a search is on its way
        if let mag = symbol("magnifyingglass", 18, .medium, l.busy ? text : .secondaryLabelColor) {
            mag.draw(in: NSRect(x: 22, y: (barH - mag.size.height) / 2, width: mag.size.width, height: mag.size.height))
        }

        let listed = !l.results.isEmpty || l.inside != nil || l.busy
        if listed {
            NSColor.separatorColor.setFill()
            NSRect(x: 20, y: barH, width: bounds.width - 40, height: 1).fill()
        }
        let numbered = l.inside?.parent.uri.hasPrefix("spotify:album:") ?? false
        if let inside = l.inside {
            draw("‹  " + inside.parent.name, at: NSRect(x: 20, y: listTop + 4, width: bounds.width - 40, height: 16), size: 11.5, weight: .semibold, color: .secondaryLabelColor)
        }
        for (i, titled, r) in rowRects() {
            let h = l.results[i]
            if titled {
                draw(h.section.uppercased(), at: NSRect(x: r.minX + 12, y: r.minY - headH + 8, width: r.width - 24, height: 14), size: 10, weight: .semibold, color: .secondaryLabelColor, kern: 0.6)
            }
            let on = i == l.picked
            if on || i == l.hover {
                text.withAlphaComponent(on ? 0.13 : 0.05).setFill()
                NSBezierPath(roundedRect: r, xRadius: 12, yRadius: 12).fill()
            }
            let t = NSRect(x: r.minX + 12, y: r.midY - thumb / 2, width: thumb, height: thumb)
            if numbered {
                draw(h.number > 0 ? "\(h.number)" : "", at: t.offsetBy(dx: 0, dy: 6), size: 12, weight: .medium, color: .tertiaryLabelColor, align: .center, mono: true)
            }
            var right = r.maxX - 12
            if on && h.opens && l.inside == nil {
                draw("›", at: NSRect(x: right - 10, y: r.midY - 11, width: 10, height: 20), size: 17, weight: .medium, color: .tertiaryLabelColor)
                right -= 18
            }
            if h.ms > 0 {
                draw(mmss(h.ms), at: NSRect(x: right - 44, y: r.midY - 8, width: 44, height: 16), size: 12, weight: .regular, color: .tertiaryLabelColor, align: .right, mono: true)
                right -= 52
            }
            let x = t.maxX + 12
            var nameW = right - x
            if h.explicit { nameW -= 18 }
            let used = draw(h.name, at: NSRect(x: x, y: r.minY + 3, width: nameW, height: 18), size: 13.5, weight: .medium, color: text)
            if h.explicit { drawChip(at: NSPoint(x: x + used + 6, y: r.minY + 6), text: text) }
            draw(h.sub, at: NSRect(x: x, y: r.minY + 20, width: right - x, height: 15), size: 11.5, weight: .regular, color: .secondaryLabelColor)
        }
        // stand-ins for Spotify's results while they're on their way
        if l.busy {
            var y = (rowRects().last.map { $0.2.maxY } ?? listTop + (l.inside != nil ? headH : 0))
            var k = 0
            while y + hitH <= listTop + room {
                text.withAlphaComponent(0.06).setFill()
                NSBezierPath(roundedRect: NSRect(x: 20, y: y + 5, width: thumb, height: thumb), xRadius: 6, yRadius: 6).fill()
                text.withAlphaComponent(0.07).setFill()
                NSBezierPath(roundedRect: NSRect(x: 60, y: y + 10, width: [180, 140, 210, 160][k % 4], height: 9), xRadius: 4.5, yRadius: 4.5).fill()
                NSBezierPath(roundedRect: NSRect(x: 60, y: y + 24, width: [110, 90, 130, 100][k % 4], height: 7), xRadius: 3.5, yRadius: 3.5).fill()
                y += hitH
                k += 1
            }
        }
        if let n = l.note {
            let y = listed ? (rowRects().last.map { $0.2.maxY + 8 } ?? listTop + (l.inside != nil ? headH : 0) + 8) : barH + 14
            draw(n, at: NSRect(x: 24, y: y, width: bounds.width - 48, height: 18), size: 13, weight: .medium, color: .secondaryLabelColor)
        }
        drawFoot(text: text)
    }

    func drawFoot(text: NSColor) {
        guard let l = launcher, let n = l.now else { return }
        let f = footRect
        text.withAlphaComponent(0.035).setFill()
        f.fill()
        NSColor.separatorColor.setFill()
        NSRect(x: 0, y: f.minY, width: f.width, height: 1).fill()
        let right = buttons.last!.0.minX - 8 - (l.copied ? 80 : 0)
        draw(n.name, at: NSRect(x: 60, y: f.minY + 9, width: right - 60, height: 17), size: 13, weight: .semibold, color: text)
        draw(n.artists, at: NSRect(x: 60, y: f.minY + 26, width: right - 60, height: 15), size: 11.5, weight: .regular, color: .secondaryLabelColor)
        if l.copied {
            draw("link copied", at: NSRect(x: right + 4, y: f.midY - 7, width: 76, height: 15), size: 11, weight: .medium, color: text)
        }
        for (r, k) in buttons {
            let (name, lit) = switch k {
            case "shuffle": ("shuffle", n.shuffle)
            case "repeat": (n.repeatMode == "one" ? "repeat.1" : "repeat", n.repeatMode != "off")
            default: (n.playing ? "pause.fill" : "play.fill", true)
            }
            // shuffle and repeat, when on, sit on a soft pill (the way Music marks them)
            if lit && k != "play" {
                NSColor.quaternaryLabelColor.setFill()
                NSBezierPath(roundedRect: r, xRadius: 8, yRadius: 8).fill()
            }
            if let s = symbol(name, 14, .semibold, lit ? text : .secondaryLabelColor) {
                s.draw(in: NSRect(x: r.midX - s.size.width / 2, y: r.midY - s.size.height / 2, width: s.size.width, height: s.size.height))
            }
        }
    }

    /// What isn't vibrant, drawn by `art` over the rest: covers and the picked row's accent.
    func drawArt() {
        guard let l = launcher else { return }
        let numbered = l.inside?.parent.uri.hasPrefix("spotify:album:") ?? false
        for (i, _, r) in rowRects() {
            let h = l.results[i]
            if i == l.picked {
                NSColor.controlAccentColor.setFill()
                NSBezierPath(roundedRect: NSRect(x: r.minX, y: r.minY + 10, width: 3, height: r.height - 20), xRadius: 1.5, yRadius: 1.5).fill()
            }
            if !numbered {
                drawThumb(l.image(h.cover), in: NSRect(x: r.minX + 12, y: r.midY - thumb / 2, width: thumb, height: thumb), round: h.round, liked: h.uri == "liked")
            }
        }
        if let n = l.now {
            let f = footRect
            drawThumb(l.image(n.cover), in: NSRect(x: 20, y: f.midY - thumb / 2, width: thumb, height: thumb), round: false, liked: false)
        }
    }

    func drawThumb(_ img: NSImage?, in r: NSRect, round: Bool, liked: Bool) {
        let rad = round ? r.width / 2 : 6
        let path = NSBezierPath(roundedRect: r, xRadius: rad, yRadius: rad)
        NSGraphicsContext.saveGraphicsState()
        path.addClip()
        if let img {
            img.draw(in: r, from: .zero, operation: .sourceOver, fraction: 1, respectFlipped: true, hints: nil)
        } else if liked {
            NSGradient(starting: NSColor(srgbRed: 0.27, green: 0.16, blue: 0.75, alpha: 1), ending: NSColor(srgbRed: 0.55, green: 0.75, blue: 0.85, alpha: 1))!.draw(in: r, angle: -45)
            if let heart = symbol("heart.fill", 12, .bold, .white) {
                heart.draw(in: NSRect(x: r.midX - heart.size.width / 2, y: r.midY - heart.size.height / 2, width: heart.size.width, height: heart.size.height))
            }
        } else {
            NSColor.quaternaryLabelColor.setFill()
            r.fill()
            if let note = symbol("music.note", 13, .regular, .tertiaryLabelColor) {
                note.draw(in: NSRect(x: r.midX - note.size.width / 2, y: r.midY - note.size.height / 2, width: note.size.width, height: note.size.height))
            }
        }
        NSGraphicsContext.restoreGraphicsState()
    }

    func drawChip(at p: NSPoint, text: NSColor) {
        let r = NSRect(x: p.x, y: p.y, width: 12, height: 12)
        NSColor.tertiaryLabelColor.setFill()
        NSBezierPath(roundedRect: r, xRadius: 2.5, yRadius: 2.5).fill()
        draw("E", at: r.offsetBy(dx: 0, dy: -0.5), size: 8.5, weight: .bold, color: launcher.dark ? .black : .white, align: .center)
    }

    /// One line of text, cut short with "…"; how wide it came out.
    @discardableResult
    func draw(_ s: String, at r: NSRect, size: CGFloat, weight: NSFont.Weight, color: NSColor, align: NSTextAlignment = .left, mono: Bool = false, kern: CGFloat = 0) -> CGFloat {
        guard r.width > 0 else { return 0 }
        let para = NSMutableParagraphStyle()
        para.lineBreakMode = .byTruncatingTail
        para.alignment = align
        let font = mono ? NSFont.monospacedDigitSystemFont(ofSize: size, weight: weight) : NSFont.systemFont(ofSize: size, weight: weight)
        let a = NSAttributedString(string: s, attributes: [.font: font, .foregroundColor: color, .paragraphStyle: para, .kern: kern])
        a.draw(with: r, options: [.usesLineFragmentOrigin, .truncatesLastVisibleLine])
        return min(a.size().width, r.width)
    }

    func symbol(_ name: String, _ size: CGFloat, _ weight: NSFont.Weight, _ color: NSColor) -> NSImage? {
        let cfg = NSImage.SymbolConfiguration(pointSize: size, weight: weight).applying(.init(paletteColors: [color]))
        return NSImage(systemSymbolName: name, accessibilityDescription: nil)?.withSymbolConfiguration(cfg)
    }
}

/// Covers and the accent, over the card and outside its vibrancy (blended, they'd wash out).
final class Art: NSView {
    weak var card: Card?
    override var isFlipped: Bool { true }
    override func hitTest(_ p: NSPoint) -> NSView? { nil }  // clicks go to the card
    override func draw(_ dirty: NSRect) { card?.drawArt() }
}

// MARK: the hotkey's spelling

func parseHotKey(_ spec: String) -> (UInt32, UInt32)? {
    var parts = spec.lowercased().split(separator: "+").map(String.init)
    guard let code = parts.popLast().flatMap({ keyCodes[$0] }) else { return nil }
    var mods = 0
    for part in parts {
        switch part {
        case "ctrl": mods |= controlKey
        case "option", "alt": mods |= optionKey
        case "cmd": mods |= cmdKey
        case "shift": mods |= shiftKey
        default: return nil
        }
    }
    return (UInt32(code), UInt32(mods))
}

let keyCodes = [
    "a": kVK_ANSI_A, "b": kVK_ANSI_B, "c": kVK_ANSI_C, "d": kVK_ANSI_D, "e": kVK_ANSI_E, "f": kVK_ANSI_F, "g": kVK_ANSI_G,
    "h": kVK_ANSI_H, "i": kVK_ANSI_I, "j": kVK_ANSI_J, "k": kVK_ANSI_K, "l": kVK_ANSI_L, "m": kVK_ANSI_M, "n": kVK_ANSI_N,
    "o": kVK_ANSI_O, "p": kVK_ANSI_P, "q": kVK_ANSI_Q, "r": kVK_ANSI_R, "s": kVK_ANSI_S, "t": kVK_ANSI_T, "u": kVK_ANSI_U,
    "v": kVK_ANSI_V, "w": kVK_ANSI_W, "x": kVK_ANSI_X, "y": kVK_ANSI_Y, "z": kVK_ANSI_Z,
    "space": kVK_Space, "return": kVK_Return,
]

let app = NSApplication.shared
let launcher = MainActor.assumeIsolated { Launcher() }
app.delegate = launcher
app.setActivationPolicy(.accessory)
app.run()
