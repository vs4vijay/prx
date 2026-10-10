// menu-capture.swift: the capture tool behind the menu bar takes in menu-clips/
// and the menu stills in docs/media/screenshots/menu*.png.
//
// The menu bar menu cannot be captured by a script alone (it needs a logged-in
// desktop, and Accessibility and Screen Recording permission for the terminal
// that runs this), so build.sh never runs this file. It is kept so the takes
// can be repeated. Read docs/development.md, "The menu bar captures", first.
//
//   swiftc -O menu-capture.swift -o /tmp/menu-capture
//
// Everything is addressed by process id; PID below is the *sandbox* PRX.app
// (scripts/demo-sandbox.sh, started with its run-app), never your own.
//
//   trusted                           are both permissions granted?
//   backdrop X Y W H HEX [LEVEL]      put a flat, click-through window on screen (runs until killed)
//   status PID | menu PID [DEPTH]     the status item / the open menu's items and frames (Accessibility)
//   item PID PATH | windows PID       one menu item's frame / the app's windows
//   pos | move X Y SECONDS | warp X Y the pointer: where it is, glide it, place it
//   clickif PID                       click, only if the element under the pointer belongs to PID
//   click | key KEYCODE               click / press a key, unconditionally (53 is Escape)
//   shot OUT X Y W H PIDS CURSOR      one PNG of a screen rectangle
//   rec2 OUT SECONDS X Y W H PIDS     a ProRes .mov of a screen rectangle at REC_FPS (default 30)
//
// PIDS is a comma-separated list; `shot` and `rec2` capture only the windows of
// those processes (the sandbox app and the backdrop), so nothing else on the
// desktop can be in a frame. For the same reason the status item itself is not
// in the takes: macOS draws it in a window the app does not own. Coordinates
// are screen points, top-left origin; output is 2 pixels per point. `rec`
// (a stream recorder) is left in for reference: streams omit menu windows.
//
// `move`, `click`, `clickif` and `key` post real input events. Use `clickif`
// to open the menu, and check `menu PID` lists only the demo data before
// recording.

import AppKit
import ApplicationServices
import ScreenCaptureKit
import AVFoundation
import ImageIO
import UniformTypeIdentifiers

let args = Array(CommandLine.arguments.dropFirst())
func die(_ m: String) -> Never { FileHandle.standardError.write((m + "\n").data(using: .utf8)!); exit(1) }
func d(_ i: Int) -> Double { Double(args[i])! }

final class FreeWindow: NSWindow { override func constrainFrameRect(_ f: NSRect, to s: NSScreen?) -> NSRect { f } }
// ---------- AX helpers
func attr(_ e: AXUIElement, _ name: String) -> AnyObject? {
    var v: AnyObject?
    return AXUIElementCopyAttributeValue(e, name as CFString, &v) == .success ? v : nil
}
func frame(_ e: AXUIElement) -> CGRect? {
    guard let p = attr(e, "AXPosition"), let s = attr(e, "AXSize") else { return nil }
    var pt = CGPoint.zero, sz = CGSize.zero
    AXValueGetValue(p as! AXValue, .cgPoint, &pt); AXValueGetValue(s as! AXValue, .cgSize, &sz)
    return CGRect(origin: pt, size: sz)
}
func kids(_ e: AXUIElement) -> [AXUIElement] { (attr(e, "AXChildren") as? [AXUIElement]) ?? [] }
func str(_ e: AXUIElement, _ n: String) -> String { (attr(e, n) as? String) ?? "" }
func statusItems(_ pid: pid_t) -> [AXUIElement] {
    let app = AXUIElementCreateApplication(pid)
    AXUIElementSetMessagingTimeout(app, 2.0)
    guard let bar = attr(app, "AXExtrasMenuBar") else { return [] }
    return kids(bar as! AXUIElement)
}
func fr(_ r: CGRect?) -> String { guard let r = r else { return "-" }; return "\(Int(r.minX)),\(Int(r.minY)),\(Int(r.width)),\(Int(r.height))" }
func dumpMenu(_ menu: AXUIElement, _ depth: Int, _ maxDepth: Int, _ path: String) {
    for (i, item) in kids(menu).enumerated() {
        let t = str(item, "AXTitle")
        let en = (attr(item, "AXEnabled") as? Bool) ?? false
        let sel = (attr(item, "AXSelected") as? Bool) ?? false
        let mark = str(item, "AXMenuItemMarkChar")
        let cmd = str(item, "AXMenuItemCmdChar")
        let sub = kids(item)
        let p = path.isEmpty ? "\(i)" : "\(path).\(i)"
        print("\(String(repeating: "  ", count: depth))[\(p)] \(t.isEmpty ? "----" : t)\(en ? "" : " (disabled)")\(sel ? " <SELECTED>" : "")\(mark.isEmpty ? "" : " mark=\(mark)")\(cmd.isEmpty ? "" : " key=\(cmd)")\(sub.isEmpty ? "" : " >")  @\(fr(frame(item)))")
        if depth < maxDepth, let m = sub.first { dumpMenu(m, depth + 1, maxDepth, p) }
    }
}
func itemAt(_ pid: pid_t, _ path: String) -> AXUIElement? {
    guard let si = statusItems(pid).first, var menu = kids(si).first else { return nil }
    var item: AXUIElement? = nil
    for part in path.split(separator: ".") {
        let ks = kids(menu); guard let i = Int(part), i < ks.count else { return nil }
        item = ks[i]
        if let m = kids(ks[i]).first { menu = m }
    }
    return item
}

// ---------- mouse
func curPos() -> CGPoint { CGEvent(source: nil)!.location }
func post(_ type: CGEventType, _ p: CGPoint) {
    let e = CGEvent(mouseEventSource: CGEventSource(stateID: .hidSystemState), mouseType: type, mouseCursorPosition: p, mouseButton: .left)!
    e.post(tap: .cghidEventTap)
}
func glide(to: CGPoint, dur: Double) {
    let from = curPos(); let n = max(1, Int(dur * 120))
    for i in 1...n {
        let t = Double(i) / Double(n); let k = t * t * (3 - 2 * t)
        post(.mouseMoved, CGPoint(x: from.x + (to.x - from.x) * k, y: from.y + (to.y - from.y) * k))
        usleep(8333)
    }
}

// ---------- capture
func makeFilter(_ pids: [pid_t]) async throws -> (SCContentFilter, SCDisplay) {
    let content = try await SCShareableContent.excludingDesktopWindows(true, onScreenWindowsOnly: false)
    let display = content.displays.first { $0.displayID == CGMainDisplayID() }!
    let apps = content.applications.filter { pids.contains($0.processID) }
    if apps.count != pids.count { FileHandle.standardError.write("warning: matched \(apps.map{$0.processID}) of \(pids)\n".data(using: .utf8)!) }
    if apps.isEmpty { die("no matching apps") }
    return (SCContentFilter(display: display, including: apps, exceptingWindows: []), display)
}
func makeConfig(_ r: CGRect, cursor: Bool, fps: Int32) -> SCStreamConfiguration {
    let c = SCStreamConfiguration()
    c.sourceRect = r
    c.width = Int(r.width * 2); c.height = Int(r.height * 2)
    c.showsCursor = cursor
    c.minimumFrameInterval = CMTime(value: 1, timescale: fps)
    c.colorSpaceName = CGColorSpace.sRGB
    c.captureResolution = .best
    c.scalesToFit = false
    c.queueDepth = 8
    return c
}
final class RecDelegate: NSObject, SCRecordingOutputDelegate {
    func recordingOutputDidStartRecording(_ o: SCRecordingOutput) { print("rec-started \(Date().timeIntervalSince1970)"); fflush(stdout) }
    func recordingOutput(_ o: SCRecordingOutput, didFailWithError e: Error) { print("rec-failed \(e)") }
    func recordingOutputDidFinishRecording(_ o: SCRecordingOutput) { print("rec-finished"); fflush(stdout) }
}
func pidList(_ s: String) -> [pid_t] { s.split(separator: ",").map { pid_t($0)! } }

guard let cmd = args.first else { die("usage") }
switch cmd {
case "trusted":
    print("ax trusted:", AXIsProcessTrusted(), "screen capture:", CGPreflightScreenCaptureAccess())
case "backdrop": // x y w h hex level
    let app = NSApplication.shared
    app.setActivationPolicy(.accessory)
    let H = NSScreen.screens[0].frame.height
    let r = NSRect(x: d(1), y: H - d(2) - d(4), width: d(3), height: d(4))
    let w = FreeWindow(contentRect: r, styleMask: [.borderless], backing: .buffered, defer: false)
    let hex = UInt32(args[5], radix: 16)!
    w.backgroundColor = NSColor(srgbRed: CGFloat((hex >> 16) & 255) / 255, green: CGFloat((hex >> 8) & 255) / 255, blue: CGFloat(hex & 255) / 255, alpha: 1)
    w.isOpaque = true; w.hasShadow = false; w.ignoresMouseEvents = true
    w.level = NSWindow.Level(rawValue: args.count > 6 ? Int(args[6])! : 2)
    w.collectionBehavior = [.canJoinAllSpaces, .stationary, .ignoresCycle, .fullScreenAuxiliary]
    w.setFrame(r, display: true)
    w.orderFrontRegardless()
    signal(SIGTERM) { _ in exit(0) }
    print("backdrop pid \(getpid()) frame \(w.frame)"); fflush(stdout)
    app.run()
case "status": // pid
    for (i, s) in statusItems(pid_t(args[1])!).enumerated() {
        print("item \(i) title='\(str(s, "AXTitle"))' desc='\(str(s, "AXDescription"))' help='\(str(s, "AXHelp"))' frame=\(fr(frame(s))) menus=\(kids(s).count)")
    }
case "menu": // pid maxDepth
    guard let si = statusItems(pid_t(args[1])!).first else { die("no status item") }
    guard let m = kids(si).first else { die("no menu") }
    print("menu frame \(fr(frame(m)))")
    dumpMenu(m, 0, args.count > 2 ? Int(args[2])! : 0, "")
case "item": // pid path -> frame + title
    guard let it = itemAt(pid_t(args[1])!, args[2]) else { die("no item") }
    print("\(fr(frame(it)))|\(str(it, "AXTitle"))")
    if let m = kids(it).first { print("submenu \(fr(frame(m)))") }
case "press": // pid [path]  (AXPress, may block while menu tracks)
    let pid = pid_t(args[1])!
    let el: AXUIElement? = args.count > 2 ? itemAt(pid, args[2]) : statusItems(pid).first
    guard let e = el else { die("no element") }
    print("press ->", AXUIElementPerformAction(e, "AXPress" as CFString).rawValue)
case "cancel": // pid
    guard let si = statusItems(pid_t(args[1])!).first, let m = kids(si).first else { die("no menu") }
    print("cancel ->", AXUIElementPerformAction(m, "AXCancel" as CFString).rawValue)
case "button": // pid title -> frame of that button in any app window, plus window frames and static texts
    let app = AXUIElementCreateApplication(pid_t(args[1])!)
    AXUIElementSetMessagingTimeout(app, 2.0)
    for w in (attr(app, "AXWindows") as? [AXUIElement]) ?? [] {
        print("window \(fr(frame(w))) '\(str(w, "AXTitle"))'")
        func walk(_ e: AXUIElement, _ depth: Int) {
            for k in kids(e) {
                let role = str(k, "AXRole")
                if role == "AXButton" { print("button '\(str(k, "AXTitle"))' \(fr(frame(k)))") }
                if role == "AXStaticText" { print("text '\((attr(k, "AXValue") as? String) ?? "")'") }
                if depth < 4 { walk(k, depth + 1) }
            }
        }
        walk(w, 0)
    }
case "windows": // pid
    let list = CGWindowListCopyWindowInfo([.optionAll], kCGNullWindowID) as! [[String: Any]]
    for w in list where (w[kCGWindowOwnerPID as String] as? Int) == Int(args[1])! {
        let b = w[kCGWindowBounds as String] as! [String: Any]
        print("id=\(w[kCGWindowNumber as String]!) layer=\(w[kCGWindowLayer as String]!) onscreen=\(w[kCGWindowIsOnscreen as String] ?? 0) bounds=\(b["X"]!),\(b["Y"]!),\(b["Width"]!),\(b["Height"]!) alpha=\(w[kCGWindowAlpha as String]!)")
    }
case "pos":
    let p = curPos(); print("\(p.x) \(p.y)")
case "move": // x y dur
    glide(to: CGPoint(x: d(1), y: d(2)), dur: d(3))
case "warp": // x y  (no events)
    CGWarpMouseCursorPosition(CGPoint(x: d(1), y: d(2)))
case "clickif": // pid : click at the pointer only if the element under it belongs to pid
    let p = curPos(); var el: AXUIElement?; var owner: pid_t = -1
    if AXUIElementCopyElementAtPosition(AXUIElementCreateSystemWide(), Float(p.x), Float(p.y), &el) == .success, let e = el { AXUIElementGetPid(e, &owner) }
    if owner == pid_t(args[1])! { post(.leftMouseDown, p); usleep(70000); post(.leftMouseUp, p); print("clicked") } else { print("refused owner=\(owner)"); exit(3) }
case "click":
    let p = curPos(); post(.leftMouseDown, p); usleep(70000); post(.leftMouseUp, p)
case "key": // keycode
    let src = CGEventSource(stateID: .hidSystemState)
    let k = CGKeyCode(args[1])!
    CGEvent(keyboardEventSource: src, virtualKey: k, keyDown: true)!.post(tap: .cghidEventTap); usleep(50000)
    CGEvent(keyboardEventSource: src, virtualKey: k, keyDown: false)!.post(tap: .cghidEventTap)
case "front":
    let a = NSWorkspace.shared.frontmostApplication; print("\(a?.processIdentifier ?? -1) \(a?.bundleIdentifier ?? "?")")
case "activate": // pid
    print(NSRunningApplication(processIdentifier: pid_t(args[1])!)?.activate(options: []) ?? false)
case "shot": // out x y w h pids cursor
    alarm(12)
    let r = CGRect(x: d(2), y: d(3), width: d(4), height: d(5))
    let sem = DispatchSemaphore(value: 0)
    Task {
        do {
            let (f, _) = try await makeFilter(pidList(args[6]))
            let c = makeConfig(r, cursor: args.count > 7 && args[7] == "1", fps: 60)
            let img = try await SCScreenshotManager.captureImage(contentFilter: f, configuration: c)
            let dst = CGImageDestinationCreateWithURL(URL(fileURLWithPath: args[1]) as CFURL, UTType.png.identifier as CFString, 1, nil)!
            CGImageDestinationAddImage(dst, img, nil); CGImageDestinationFinalize(dst)
            print("shot \(img.width)x\(img.height)")
        } catch { print("shot failed: \(error)") }
        sem.signal()
    }
    sem.wait()
case "rec2": // out seconds x y w h pids  -- screenshot-loop recorder (streams omit menu windows)
    alarm(UInt32(d(2)) + 15)
    let r = CGRect(x: d(3), y: d(4), width: d(5), height: d(6))
    let want = pidList(args[7])
    let sem = DispatchSemaphore(value: 0)
    Task {
        do {
            func current() async throws -> SCContentFilter? {
                let content = try await SCShareableContent.excludingDesktopWindows(true, onScreenWindowsOnly: false)
                let display = content.displays.first { $0.displayID == CGMainDisplayID() }!
                let apps = content.applications.filter { want.contains($0.processID) }
                if apps.isEmpty { return nil }
                return SCContentFilter(display: display, including: apps, exceptingWindows: [])
            }
            func winSet() -> [Int] {
                let list = CGWindowListCopyWindowInfo([.optionOnScreenOnly], kCGNullWindowID) as! [[String: Any]]
                return list.filter { want.contains(pid_t($0[kCGWindowOwnerPID as String] as! Int)) }.map { $0[kCGWindowNumber as String] as! Int }.sorted()
            }
            guard var f = try await current() else { die("no matching apps") }; var seen = winSet()
            let c = makeConfig(r, cursor: true, fps: 60)
            let W = Int(r.width * 2), H = Int(r.height * 2)
            try? FileManager.default.removeItem(atPath: args[1])
            let writer = try AVAssetWriter(outputURL: URL(fileURLWithPath: args[1]), fileType: .mov)
            let input = AVAssetWriterInput(mediaType: .video, outputSettings: [AVVideoCodecKey: AVVideoCodecType.proRes422, AVVideoWidthKey: W, AVVideoHeightKey: H])
            input.expectsMediaDataInRealTime = true
            let adaptor = AVAssetWriterInputPixelBufferAdaptor(assetWriterInput: input, sourcePixelBufferAttributes: [kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_32BGRA, kCVPixelBufferWidthKey as String: W, kCVPixelBufferHeightKey as String: H])
            writer.add(input); writer.startWriting(); writer.startSession(atSourceTime: .zero)
            let cs = CGColorSpace(name: CGColorSpace.sRGB)!
            let start = Date(); var n = 0
            print("rec-started \(start.timeIntervalSince1970)"); fflush(stdout)
            let fpsT = Double(ProcessInfo.processInfo.environment["REC_FPS"] ?? "30")!
            let (taskStream, cont) = AsyncStream<Task<(Double, CGImage)?, Never>>.makeStream()
            let consumer = Task {
                for await tk in taskStream {
                    guard let (t, img) = await tk.value else { continue }
                    var pb: CVPixelBuffer?
                    CVPixelBufferPoolCreatePixelBuffer(nil, adaptor.pixelBufferPool!, &pb)
                    guard let buf = pb else { continue }
                    CVPixelBufferLockBaseAddress(buf, [])
                    let ctx = CGContext(data: CVPixelBufferGetBaseAddress(buf), width: W, height: H, bitsPerComponent: 8, bytesPerRow: CVPixelBufferGetBytesPerRow(buf), space: cs, bitmapInfo: CGImageAlphaInfo.premultipliedFirst.rawValue | CGBitmapInfo.byteOrder32Little.rawValue)!
                    ctx.setFillColor(CGColor(red: 0, green: 0, blue: 0, alpha: 1)); ctx.fill(CGRect(x: 0, y: 0, width: W, height: H))
                    ctx.draw(img, in: CGRect(x: 0, y: 0, width: W, height: H))
                    CVPixelBufferUnlockBaseAddress(buf, [])
                    while !input.isReadyForMoreMediaData { try? await Task.sleep(nanoseconds: 2_000_000) }
                    adaptor.append(buf, withPresentationTime: CMTime(seconds: t, preferredTimescale: 600)); n += 1
                }
            }
            var k = 0
            while Date().timeIntervalSince(start) < d(2) {
                let now = winSet()
                if now != seen { if let nf = try? await current() { f = nf; seen = now } }
                let t = Double(k) / fpsT; k += 1
                let ff = f
                cont.yield(Task { try? await (t, SCScreenshotManager.captureImage(contentFilter: ff, configuration: c)) })
                let wait = start.addingTimeInterval(Double(k) / fpsT).timeIntervalSinceNow
                if wait > 0 { try await Task.sleep(nanoseconds: UInt64(wait * 1e9)) }
            }
            cont.finish(); await consumer.value
            input.markAsFinished(); await writer.finishWriting()
            print("rec-finished frames=\(n) fps=\(Double(n) / d(2))")
        } catch { print("rec2 failed: \(error)") }
        sem.signal()
    }
    sem.wait()
case "rec": // out seconds x y w h pids fps
    alarm(UInt32(d(2)) + 10)
    let r = CGRect(x: d(3), y: d(4), width: d(5), height: d(6))
    let del = RecDelegate()
    let sem = DispatchSemaphore(value: 0)
    Task {
        do {
            let want = pidList(args[7])
            func current() async throws -> (SCContentFilter, [pid_t]) {
                let content = try await SCShareableContent.excludingDesktopWindows(true, onScreenWindowsOnly: false)
                let display = content.displays.first { $0.displayID == CGMainDisplayID() }!
                let apps = content.applications.filter { want.contains($0.processID) }
                let flt = SCContentFilter(display: display, including: apps, exceptingWindows: [])
                if ProcessInfo.processInfo.environment["REC_MENUBAR"] == "1" { flt.includeMenuBar = true }
                return (flt, apps.map { $0.processID }.sorted())
            }
            var (f, have) = try await current()
            if have.isEmpty { die("no matching apps") }
            let c = makeConfig(r, cursor: true, fps: Int32(args.count > 8 ? args[8] : "60")!)
            let stream = SCStream(filter: f, configuration: c, delegate: nil)
            let rc = SCRecordingOutputConfiguration()
            rc.outputURL = URL(fileURLWithPath: args[1]); rc.outputFileType = .mov; rc.videoCodecType = .hevc
            let ro = SCRecordingOutput(configuration: rc, delegate: del)
            try stream.addRecordingOutput(ro)
            try await stream.startCapture()
            let end = Date().addingTimeInterval(d(2))
            func winSet() -> [Int] {
                let list = CGWindowListCopyWindowInfo([.optionOnScreenOnly], kCGNullWindowID) as! [[String: Any]]
                return list.filter { want.contains(pid_t($0[kCGWindowOwnerPID as String] as! Int)) }.map { $0[kCGWindowNumber as String] as! Int }.sorted()
            }
            var seen = winSet()
            while Date() < end {
                try await Task.sleep(nanoseconds: 16_000_000)
                let now = winSet()
                if now != seen, let (nf, nh) = try? await current() {
                    seen = now
                    try await stream.updateContentFilter(nf); have = nh; f = nf
                    print("filter-updated \(now.count) windows \(Date().timeIntervalSince1970)"); fflush(stdout)
                }
            }
            try await stream.stopCapture()
            try await Task.sleep(nanoseconds: 500_000_000)
        } catch { print("rec failed: \(error)") }
        sem.signal()
    }
    sem.wait()
default: die("unknown \(cmd)")
}
