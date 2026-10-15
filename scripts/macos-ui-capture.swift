import AppKit
import SwiftUI

// Scripted screenshot tour of the real Mac app views, driven through the same
// Recovery model and bundled engine. Built by scripts/capture-macos-ui.sh.
// Captures only this process's own off-screen windows, so no Screen Recording
// permission is needed and nothing appears on the user's display.

final class CaptureWindow: NSWindow {
    override func constrainFrameRect(_ frameRect: NSRect, to screen: NSScreen?) -> NSRect { frameRect }
    // Draw as the active window even though this background tool never takes focus.
    override var isKeyWindow: Bool { true }
    override var isMainWindow: Bool { true }
    override var canBecomeKey: Bool { true }
}

@MainActor enum Tour {
    static var window: CaptureWindow!
    static let output = URL(fileURLWithPath: CommandLine.arguments[1])
    static let env = ProcessInfo.processInfo.environment

    static func pause(_ seconds: Double = 0.7) async { try? await Task.sleep(nanoseconds: UInt64(seconds * 1e9)) }
    static func settle(_ model: Recovery) async {
        await pause(0.25)
        while model.busy { await pause(0.05) }
        await pause(0.8)
    }

    static func snap(_ name: String) {
        window.makeKeyAndOrderFront(nil)
        window.displayIfNeeded()
        // CGWindowListCreateImageFromArray is obsoleted in newer SDKs but still
        // exported; it composites this window with any attached sheet.
        typealias FromArray = @convention(c) (CGRect, CFArray, UInt32) -> Unmanaged<CGImage>?
        guard let symbol = dlsym(UnsafeMutableRawPointer(bitPattern: -2), "CGWindowListCreateImageFromArray") else {
            fatalError("Window capture is unavailable on this macOS version")
        }
        let capture = unsafeBitCast(symbol, to: FromArray.self)
        var ids: [CGWindowID] = []
        if let sheet = window.attachedSheet { ids.append(CGWindowID(sheet.windowNumber)) }
        ids.append(CGWindowID(window.windowNumber))
        var pointers = ids.map { UnsafeRawPointer(bitPattern: UInt($0)) }
        let array = CFArrayCreate(nil, &pointers, pointers.count, nil)!
        guard let image = capture(.null, array, 1 << 0 | 1 << 3)?.takeRetainedValue() else {
            print("capture failed:", name); return
        }
        let png = NSBitmapImageRep(cgImage: image).representation(using: .png, properties: [:])!
        try! png.write(to: output.appendingPathComponent("\(name).png"))
        print("captured", name)
    }

    static func run(_ model: Recovery) async {
        let fixtures = env["ZFSE_CAPTURE_FIXTURES"]!
        let inputs = env["ZFSE_CAPTURE_INPUTS"]!
        let scratch = FileManager.default.temporaryDirectory.appendingPathComponent("zfse-capture-\(UUID().uuidString)")
        try? FileManager.default.createDirectory(at: scratch, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: scratch) }
        await pause(1.5)
        snap("01-welcome")
        model.chooseDrive(); await pause(1.2); snap("02-connected-drive"); model.showDrives = false; await pause(0.8)
        model.sourcePath = "/Volumes/Backups/slide.img"; model.showPath = true; await pause(1); snap("03-open-path"); model.showPath = false; await pause(0.8)
        model.open(URL(fileURLWithPath: "\(inputs)/notes.txt")); await settle(model); snap("04-not-a-backup"); model.error = nil

        model.open(URL(fileURLWithPath: "\(fixtures)/multi-snapshot.zfs")); await settle(model); snap("05-send-newest")
        model.select(0); await settle(model); snap("06-send-oldest")
        model.selection = model.state?.entries.first(where: { $0.regular })?.name; await pause(); snap("07-file-selected")
        if let name = model.selection {
            await model.run(["method": "extract", "name": name, "destination": scratch.appendingPathComponent(name).path], activity: "Restoring \(name)…", restore: true)
            await pause(); snap("08-restored")
            await model.run(["method": "extract", "name": name, "destination": scratch.appendingPathComponent(name).path], activity: "Restoring \(name)…", restore: true)
            await pause(); snap("09-already-exists"); model.error = nil; model.restored = nil
        }

        model.open(URL(fileURLWithPath: "\(fixtures)/encrypted-raw-s1.zfs")); await settle(model); snap("10-locked")
        model.secret = "not-the-key"; model.unlock(); await settle(model); snap("11-wrong-key"); model.error = nil
        model.secret = "zfs-send-fixture-passphrase"; await pause(0.3); snap("12-key-entered")
        model.unlock(); await settle(model); snap("13-unlocked")
        model.list("/docs"); await settle(model); snap("14-folder")
        model.showPreferences = true; await pause(1); snap("15-time-zone"); model.showPreferences = false; await pause(0.8)

        model.open(URL(fileURLWithPath: "\(inputs)/ext4.img")); await settle(model); snap("16-disk-image")
        model.search = "sym"; await pause(); snap("17-search")

        if let slide = env["ZFSE_CAPTURE_SLIDE"], let key = env["ZFSE_CAPTURE_SLIDE_KEY"] {
            model.search = ""
            model.open(URL(fileURLWithPath: slide)); await settle(model); snap("20-slide-opened")
            await model.run(["method": "unlock", "key_file": key], activity: "Unlocking…"); await pause(2); snap("21-slide-unlocked")
            if let image = model.suggestedImages.first {
                model.enter(image); await settle(model); snap("22-slide-choose-volume")
                if let volume = model.state?.volumes.first(where: { $0.recommended == true }) {
                    model.chooseVolume(volume); await settle(model); snap("23-slide-volume-root")
                }
                model.list("/etc"); await settle(model)
                model.search = "hostname"; await pause()
                model.selection = "hostname"; await pause(); snap("24-slide-file-selected")
                await model.run(["method": "extract", "name": "hostname", "destination": scratch.appendingPathComponent("hostname").path], activity: "Restoring hostname…", restore: true)
                await pause(); snap("25-slide-restored")
            }
        }
        exit(0)
    }
}

@main struct CaptureMain {
    static func main() {
        let app = NSApplication.shared
        app.setActivationPolicy(.accessory)
        let env = ProcessInfo.processInfo.environment
        app.appearance = NSAppearance(named: env["ZFSE_CAPTURE_DARK"] == "1" ? .darkAqua : .aqua)
        let width = Double(env["ZFSE_CAPTURE_WIDTH"] ?? "") ?? 1180
        let height = Double(env["ZFSE_CAPTURE_HEIGHT"] ?? "") ?? 760
        MainActor.assumeIsolated {
            let model = Recovery()
            let frame = NSRect(x: -8000, y: -8000, width: width, height: height)
            let window = CaptureWindow(contentRect: frame, styleMask: [.titled, .closable, .miniaturizable, .resizable, .fullSizeContentView],
                                       backing: .buffered, defer: false)
            let host = NSHostingView(rootView: ContentView(model: model).tint(.teal))
            host.sceneBridgingOptions = [.toolbars, .title]
            window.contentView = host
            window.title = "ZFS Explore"
            window.setFrame(frame, display: true)
            window.orderFrontRegardless()
            Tour.window = window
            Task { await Tour.run(model) }
        }
        app.run()
    }
}
