import AppKit
import WebKit
import ApplicationServices

struct CheckFailure: Error, CustomStringConvertible {
    let description: String
    init(_ message: String) { description = message }
}

func require(_ condition: Bool, _ message: String) throws {
    if !condition { throw CheckFailure(message) }
}

typealias Clipboard = [[String: Data]]

@MainActor
final class InsertionFixture: NSObject, NSApplicationDelegate, NSWindowDelegate {
    let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 680, height: 640),
                          styleMask: [.titled, .closable], backing: .buffered, defer: false)
    let native = NSTextField(string: "AB")
    let secure = NSSecureTextField(string: "secret")
    let web = WKWebView(frame: .zero)
    var original: Clipboard?
    var ownedChangeCount: Int?
    var results: [[String: Any]] = []
    var evidence: [String: Any] = [:]
    var runTask: Task<Void, Never>?
    var signalSources: [DispatchSourceSignal] = []
    let reportURL: URL

    init(reportURL: URL) { self.reportURL = reportURL; super.init() }

    func applicationDidFinishLaunching(_ notification: Notification) {
        for number in [SIGINT, SIGTERM] {
            signal(number, SIG_IGN)
            let source = DispatchSource.makeSignalSource(signal: number, queue: .main)
            source.setEventHandler { self.runTask?.cancel() }
            source.resume()
            signalSources.append(source)
        }
        runTask = Task { await run() }
    }

    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        runTask?.cancel()
        return .terminateCancel
    }

    func windowShouldClose(_ sender: NSWindow) -> Bool {
        runTask?.cancel()
        return false
    }

    func pause(_ milliseconds: UInt64) async throws {
        try await Task.sleep(nanoseconds: milliseconds * 1_000_000)
    }

    func waitForPostedPaste() async throws {
        // Command-V is already in the event queue. Cancellation must not restore
        // the clipboard or close the window before the production deadline.
        await withCheckedContinuation { continuation in
            DispatchQueue.main.asyncAfter(deadline: .now() + .milliseconds(Int(ptt_test_restore_ms()))) {
                continuation.resume()
            }
        }
        try Task.checkCancellation()
    }

    func js(_ code: String) async throws -> Any {
        try Task.checkCancellation()
        let result: Any = try await withCheckedThrowingContinuation { continuation in
            var completed = false
            DispatchQueue.main.asyncAfter(deadline: .now() + 5) {
                guard !completed else { return }
                completed = true
                continuation.resume(throwing: CheckFailure("WebKit JavaScript timeout"))
            }
            web.evaluateJavaScript(code) { value, error in
                guard !completed else { return }
                completed = true
                if let error = error { continuation.resume(throwing: error) }
                else { continuation.resume(returning: value ?? NSNull()) }
            }
        }
        try Task.checkCancellation()
        return result
    }

    func eventually(_ label: String, _ condition: () async throws -> Bool) async throws {
        let deadline = Date().addingTimeInterval(5)
        while Date() < deadline {
            if try await condition() { return }
            try await pause(20)
        }
        throw CheckFailure("Timeout: \(label)")
    }

    func snapshot() throws -> Clipboard {
        try (NSPasteboard.general.pasteboardItems ?? []).map { item in
            var saved: [String: Data] = [:]
            for type in item.types {
                guard let data = item.data(forType: type) else {
                    throw CheckFailure("Unreadable clipboard representation: \(type.rawValue)")
                }
                saved[type.rawValue] = data
            }
            return saved
        }
    }

    func requireClipboardOwnership() throws {
        if let count = ownedChangeCount {
            try require(NSPasteboard.general.changeCount == count,
                        "Clipboard changed outside the fixture; stopping without overwriting it")
        }
    }

    func writeClipboard(_ contents: Clipboard) throws {
        try requireClipboardOwnership()
        let items = try contents.map { saved -> NSPasteboardItem in
            let item = NSPasteboardItem()
            for (type, data) in saved {
                let written = data.withUnsafeBytes {
                    fixtureSetClipboardBytes(item, $0.baseAddress, UInt($0.count), type)
                }
                try require(written, "Cannot prepare clipboard")
            }
            return item
        }
        NSPasteboard.general.clearContents()
        ownedChangeCount = NSPasteboard.general.changeCount
        if !items.isEmpty {
            let written = NSPasteboard.general.writeObjects(items)
            ownedChangeCount = NSPasteboard.general.changeCount
            try require(written, "Cannot write fixture clipboard")
        }
    }

    func seedClipboard() throws -> Clipboard {
        let image = NSImage(size: NSSize(width: 2, height: 2))
        image.lockFocus()
        NSColor.blue.setFill()
        NSRect(x: 0, y: 0, width: 2, height: 2).fill()
        image.unlockFocus()
        guard let tiff = image.tiffRepresentation else { throw CheckFailure("Cannot create TIFF") }
        try writeClipboard([
            [NSPasteboard.PasteboardType.string.rawValue: Data("original clipboard".utf8),
             NSPasteboard.PasteboardType.rtf.rawValue: Data("{\\rtf1\\ansi rich clipboard}".utf8),
             "com.ptt2me.fixture.binary": Data([0, 1, 0, 255])],
            [NSPasteboard.PasteboardType.tiff.rawValue: tiff],
            [NSPasteboard.PasteboardType.fileURL.rawValue: Data(reportURL.absoluteString.utf8)]
        ])
        return try snapshot()
    }

    func requireOwnFocus() throws {
        try require(NSWorkspace.shared.frontmostApplication?.processIdentifier == getpid(),
                    "Focus left the fixture; refusing to post Command-V")
        var focused: CFTypeRef?
        let error = AXUIElementCopyAttributeValue(AXUIElementCreateSystemWide(),
                                                kAXFocusedUIElementAttribute as CFString, &focused)
        try require(error == .success && focused != nil, "AX focus is unavailable: \(error.rawValue)")
        let element = focused as! AXUIElement
        var pid: pid_t = 0
        try require(AXUIElementGetPid(element, &pid) == .success && pid == getpid(),
                    "AX focus process \(pid) differs from fixture \(getpid())")
    }

    func resetFields() async throws {
        native.stringValue = "AB"
        secure.stringValue = "secret"
        _ = try await js("resetFields()")
    }

    func focus(_ field: String) async throws {
        try require(NSApp.isActive, "Fixture is no longer active")
        if field == "native" || field == "nativeSecure" {
            let control: NSTextField = field == "native" ? native : secure
            try require(window.makeFirstResponder(control), "Cannot focus \(field)")
            guard let editor = control.currentEditor() else { throw CheckFailure("No field editor") }
            editor.selectedRange = NSRange(location: 1, length: 0)
        } else {
            try require(window.makeFirstResponder(web), "Cannot focus WebKit")
            let selected = try await js("focusField('\(field)')") as? String
            try require(selected == field, "Wrong DOM focus")
        }
        // WebKit publishes AX focus asynchronously, including on its first AX query.
        var focusError: Error = CheckFailure("AX focus did not become available")
        do {
            try await eventually("AX focus for \(field)") {
                do { try self.requireOwnFocus(); return true }
                catch { focusError = error; return false }
            }
        } catch { throw focusError }
    }

    func value(_ field: String) async throws -> String {
        if field == "native" { return native.currentEditor()?.string ?? native.stringValue }
        if field == "nativeSecure" { return secure.currentEditor()?.string ?? secure.stringValue }
        guard let value = try await js("fieldValue('\(field)')") as? String else {
            throw CheckFailure("Missing field value: \(field)")
        }
        return value
    }

    func begin(_ appendSpace: Bool = true) throws -> UnsafeMutableRawPointer {
        try requireOwnFocus()
        try requireClipboardOwnership()
        var handle: UnsafeMutableRawPointer?
        let code = ptt_test_begin(" Привет. ", appendSpace, &handle)
        ownedChangeCount = NSPasteboard.general.changeCount
        try require(code == 0 && handle != nil, "Production begin failed: \(code)")
        return handle!
    }

    func finish(_ handle: UnsafeMutableRawPointer) throws {
        // Always consume the Rust handle, even when a check fails; its own
        // changeCount guard preserves a newer clipboard.
        let stillOwned = ownedChangeCount == NSPasteboard.general.changeCount
        let code = ptt_test_finish(handle)
        if stillOwned { ownedChangeCount = NSPasteboard.general.changeCount }
        try require(code == 0, "Production clipboard restore failed: \(code)")
        try require(stillOwned, "External clipboard change during insertion")
    }

    func insert(from: String, to: String? = nil, appendSpace: Bool = true,
                newClipboard: Bool = false) async throws {
        try await resetFields()
        let before = try seedClipboard()
        try await focus(from)
        var handle: UnsafeMutableRawPointer? = try begin(appendSpace)
        defer { if let remaining = handle { try? finish(remaining) } }
        if let to = to { try await focus(to) }
        try await pause(ptt_test_settle_ms())
        try requireOwnFocus()
        try requireClipboardOwnership()
        try require(ptt_test_paste(handle!) == 0, "Production paste failed")
        // Use the production restore deadline, not a longer test-only clipboard lifetime.
        try await waitForPostedPaste()
        if newClipboard {
            try writeClipboard([[NSPasteboard.PasteboardType.string.rawValue: Data("new copy".utf8)]])
        }
        let expectedClipboard = newClipboard ? try snapshot() : before
        let finishing = handle!
        handle = nil
        try finish(finishing)
        try require(try snapshot() == expectedClipboard, "Clipboard item/representation loss")
        let target = to ?? from
        let expected = appendSpace ? "AПривет. B" : "AПривет.B"
        // WebKit can preserve a pasted trailing space in a whitespace-collapsing
        // contenteditable with NBSP. Accept only these two exact DOM strings;
        // native/input/textarea still require the literal ASCII space.
        let allowed = target == "editable" && appendSpace
            ? [expected, "AПривет.\u{00a0}B"] : [expected]
        do {
            try await eventually("final text in \(target)") { try await allowed.contains(self.value(target)) }
        } catch {
            let actual = try await value(target)
            throw CheckFailure("Final text in \(target): expected \(expected.debugDescription), actual \(actual.debugDescription), Unicode \(actual.unicodeScalars.map { String($0.value, radix: 16) })")
        }
        if to != nil && from != target {
            try require(try await value(from) == "AB", "Text also appeared in the previous field")
        }
        if !target.hasPrefix("native") {
            let count = try await js("inputEvents['\(target)']") as? Int
            try require(count == 1, "Expected one real input event in \(target), got \(String(describing: count))")
        }
        evidence = ["field": target, "actual_text": try await value(target),
                    "allowed_text": allowed, "clipboard_preserved": true]
    }

    func rejectSecure(_ field: String, afterBegin: Bool) async throws {
        try await resetFields()
        let before = try seedClipboard()
        if afterBegin {
            try await focus("native")
            var handle: UnsafeMutableRawPointer? = try begin()
            defer { if let remaining = handle { try? finish(remaining) } }
            try await focus(field)
            try requireOwnFocus()
            try require(ptt_test_paste(handle!) == 1, "Secure focus at paste time was not rejected")
            let finishing = handle!
            handle = nil
            try finish(finishing)
        } else {
            try await focus(field)
            let changeCount = NSPasteboard.general.changeCount
            var handle: UnsafeMutableRawPointer?
            let code = ptt_test_begin("Привет.", false, &handle)
            // Even a regression that incorrectly allows a secure begin must
            // release the Rust transaction and restore the user's clipboard.
            ownedChangeCount = NSPasteboard.general.changeCount
            defer { if let handle = handle { try? finish(handle) } }
            try require(code == 1 && handle == nil, "Secure field was not rejected at begin")
            try require(NSPasteboard.general.changeCount == changeCount, "Secure begin touched clipboard")
        }
        try await pause(ptt_test_restore_ms())
        try require(try await value(field) == "secret", "Password field changed")
        try require(try await value("native") == "AB", "Text leaked to previous native field")
        try require(try snapshot() == before, "Secure rejection lost clipboard data")
        evidence = ["field": field, "actual_text": try await value(field),
                    "secure_rejected": true, "clipboard_preserved": true]
    }

    func check(_ name: String, _ body: () async throws -> Void) async throws {
        do {
            evidence = [:]
            try await body()
            results.append(["name": name, "status": "PASS", "evidence": evidence])
            print("PASS: \(name)")
        } catch {
            results.append(["name": name, "status": "FAIL", "error": String(describing: error)])
            throw error
        }
    }

    func prepare() async throws {
        original = try snapshot()
        ownedChangeCount = NSPasteboard.general.changeCount
        window.title = "PTT2me — automated insertion checks"
        window.isReleasedWhenClosed = false
        window.delegate = self
        let stack = NSStackView(views: [NSTextField(labelWithString: "Native text / password"), native, secure, web])
        stack.orientation = .vertical
        stack.spacing = 10
        stack.edgeInsets = NSEdgeInsets(top: 16, left: 16, bottom: 16, right: 16)
        stack.translatesAutoresizingMaskIntoConstraints = false
        window.contentView!.addSubview(stack)
        NSLayoutConstraint.activate([
            stack.leadingAnchor.constraint(equalTo: window.contentView!.leadingAnchor),
            stack.trailingAnchor.constraint(equalTo: window.contentView!.trailingAnchor),
            stack.topAnchor.constraint(equalTo: window.contentView!.topAnchor),
            stack.bottomAnchor.constraint(equalTo: window.contentView!.bottomAnchor),
            native.widthAnchor.constraint(equalTo: stack.widthAnchor, constant: -32),
            secure.widthAnchor.constraint(equalTo: native.widthAnchor),
            web.widthAnchor.constraint(equalTo: native.widthAnchor), web.heightAnchor.constraint(greaterThanOrEqualToConstant: 440)
        ])
        // Command-V must travel through the ordinary responder-chain paste action.
        let menu = NSMenu()
        let edit = NSMenuItem(title: "Edit", action: nil, keyEquivalent: "")
        let submenu = NSMenu(title: "Edit")
        submenu.addItem(withTitle: "Paste", action: #selector(NSText.paste(_:)), keyEquivalent: "v")
        edit.submenu = submenu
        menu.addItem(edit)
        NSApp.mainMenu = menu
        let htmlURL = Bundle.main.url(forResource: "fields", withExtension: "html")!
        web.loadFileURL(htmlURL, allowingReadAccessTo: htmlURL.deletingLastPathComponent())
        window.center()
        window.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
        do {
            try await eventually("fixture activation") { NSApp.isActive && self.window.isKeyWindow }
        } catch {
            throw CheckFailure("Activation failed: active=\(NSApp.isActive), key=\(window.isKeyWindow), frontmost=\(NSWorkspace.shared.frontmostApplication?.processIdentifier == getpid())")
        }
        try await eventually("WebKit document") {
            if self.web.isLoading { return false }
            return (try await self.js("typeof focusField === 'function'")) as? Bool == true
        }
    }

    func run() async {
        var failure: String?
        var originalClipboardRestored = false
        do {
            try await prepare()
            for field in ["native", "input", "textarea", "editable"] {
                try await check("insert_\(field)") { try await self.insert(from: field) }
            }
            try await check("append_space_disabled") { try await self.insert(from: "textarea", appendSpace: false) }
            try await check("focus_native_to_web") { try await self.insert(from: "native", to: "input") }
            try await check("focus_web_to_native") { try await self.insert(from: "editable", to: "native") }
            for field in ["nativeSecure", "password"] {
                try await check("reject_\(field)_at_begin") { try await self.rejectSecure(field, afterBegin: false) }
                try await check("reject_\(field)_at_paste") { try await self.rejectSecure(field, afterBegin: true) }
            }
            try await check("preserve_new_copy") { try await self.insert(from: "textarea", newClipboard: true) }
        } catch { failure = String(describing: error) }
        if let original = original {
            do {
                try writeClipboard(original)
                try require(try snapshot() == original, "Original clipboard was not restored")
                originalClipboardRestored = true
            }
            catch { failure = failure ?? String(describing: error) }
        }
        window.close()
        web.stopLoading()
        let report: [String: Any] = [
            "status": failure == nil ? "PASS" : "FAIL", "checks": results,
            "error": failure ?? "", "macOS": ProcessInfo.processInfo.operatingSystemVersionString,
            "original_clipboard_restored": originalClipboardRestored,
            "time": ISO8601DateFormatter().string(from: Date()),
            "scope": "Production insertion modules in AppKit and WKWebView; not a manual release gate"
        ]
        do {
            let data = try JSONSerialization.data(withJSONObject: report, options: [.prettyPrinted, .sortedKeys])
            try data.write(to: reportURL, options: .atomic)
        } catch { failure = "Cannot save report: \(error)" }
        if let failure = failure { fputs("FAIL: \(failure)\n", stderr) }
        exit(failure == nil ? 0 : 1)
    }
}

// This is an access preflight. --run separately proves active window/AX focus;
// a session dictionary alone does not prove that the display is unlocked.
guard AXIsProcessTrusted(), CGPreflightPostEventAccess(),
      let session = CGSessionCopyCurrentDictionary() as? [String: Any],
      session[kCGSessionOnConsoleKey as String] as? Bool == true else {
    fputs("BLOCKED: requires an on-console macOS session, Accessibility and event-posting access for this fixture. No TCC settings were changed.\n", stderr)
    exit(2)
}
if CommandLine.arguments.contains("--preflight") {
    print("Access preflight PASS; active GUI focus is checked during --run")
    exit(0)
}
guard CommandLine.arguments.count == 2 else {
    fputs("Usage: InsertionFixture <report.json> | --preflight\n", stderr)
    exit(2)
}
MainActor.assumeIsolated {
let application = NSApplication.shared
application.setActivationPolicy(.regular)
let fixture = InsertionFixture(reportURL: URL(fileURLWithPath: CommandLine.arguments[1]))
application.delegate = fixture
application.run()

}
