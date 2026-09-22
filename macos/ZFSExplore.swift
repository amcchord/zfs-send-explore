import AppKit
import SwiftUI

struct BackupView: Decodable {
    let label: String; let encrypted: Bool; let key_format: String?
    let created_at: UInt64?; let selector: String
}
struct Entry: Decodable, Identifiable {
    let name: String; let directory: Bool; let regular: Bool; let size: UInt64?
    var id: String { name }
    var image: Bool { ["raw", "img", "vhd", "vmdk", "qcow2", "datto"].contains(URL(fileURLWithPath: name).pathExtension.lowercased()) }
    var icon: String { directory ? "folder.fill" : image ? "externaldrive.fill" : "doc" }
    var detail: String { directory ? "Folder" : !regular ? "Special file" : image ? "Disk image" : "File" }
}
struct Volume: Decodable, Identifiable {
    let selector: String; let label: String; let supported: Bool; let bytes: UInt64; let filesystem: String?
    var id: String { selector }
    var display: String { "\(selector) · \(filesystem ?? "Unsupported") · \(ByteCountFormatter.string(fromByteCount: Int64(clamping: bytes), countStyle: .file))" }
}
struct BrowserState: Decodable {
    let title: String; let summary: String?; let views: [BackupView]; let view: Int
    let locked: Bool; let path: String; let entries: [Entry]; let volumes: [Volume]
    let volume: String?; let layers: [String]; let can_back: Bool
}
struct RestoreResult: Decodable { let restored: String; let bytes: UInt64; let files: UInt64; let skipped: UInt64; let sha256: String? }
struct ServiceError: LocalizedError { let message: String; var errorDescription: String? { message } }

/// Serial private pipe to the shared Rust engine. Blocking work never runs on the main actor.
final class Engine: @unchecked Sendable {
    private let queue = DispatchQueue(label: "tech.zfs-explore.engine", qos: .userInitiated)
    private var process: Process?
    private var input: FileHandle?
    private var output: FileHandle?
    private var pending = Data()
    func request(_ data: Data) async throws -> Data {
        try await withCheckedThrowingContinuation { continuation in
            queue.async {
                do {
                    if self.process == nil {
                        let process = Process()
                        guard let binary = Bundle.main.url(forAuxiliaryExecutable: "zfs-explore-service") else {
                            throw ServiceError(message: "The recovery engine is missing. Reinstall the complete app bundle.")
                        }
                        process.executableURL = binary
                        let input = Pipe(), output = Pipe()
                        process.standardInput = input; process.standardOutput = output
                        process.standardError = FileHandle.nullDevice
                        try process.run()
                        self.process = process; self.input = input.fileHandleForWriting; self.output = output.fileHandleForReading
                    }
                    try self.input?.write(contentsOf: data + Data([10]))
                    while !self.pending.contains(10) {
                        guard let chunk = self.output?.availableData, !chunk.isEmpty else {
                            throw ServiceError(message: "The recovery engine stopped. Close and reopen the app to try again.")
                        }
                        self.pending.append(chunk)
                    }
                    let end = self.pending.firstIndex(of: 10)!
                    let response = self.pending.prefix(upTo: end)
                    self.pending.removeSubrange(...end)
                    guard let envelope = try JSONSerialization.jsonObject(with: response) as? [String: Any] else {
                        throw ServiceError(message: "The recovery engine returned an invalid response.")
                    }
                    if envelope["ok"] as? Bool != true {
                        throw ServiceError(message: envelope["error"] as? String ?? "The operation failed.")
                    }
                    continuation.resume(returning: try JSONSerialization.data(withJSONObject: envelope["result"]!))
                } catch { continuation.resume(throwing: error) }
            }
        }
    }
    deinit { try? input?.close(); process?.terminate() }
}

@MainActor final class Recovery: ObservableObject {
    @Published var state: BrowserState?
    @Published var selection: String?
    @Published var busy = false
    @Published var activity = ""
    @Published var error: String?
    @Published var restored: RestoreResult?
    @Published var search = ""
    @Published var secret = ""
    @Published var showUnlock = false
    @Published var showPath = false
    @Published var showPreferences = false
    @Published var sourcePath = ""
    private let engine = Engine()
    var selected: Entry? { state?.entries.first { $0.name == selection } }
    var entries: [Entry] { state?.entries.filter { search.isEmpty || $0.name.localizedCaseInsensitiveContains(search) } ?? [] }
    func perform(_ request: [String: Any], activity: String, restore: Bool = false) {
        guard !busy else { return }
        busy = true; self.activity = activity; error = nil
        restored = nil
        do {
            let data = try JSONSerialization.data(withJSONObject: request)
            Task {
                defer { busy = false }
                do {
                    let result = try await engine.request(data)
                    if restore { restored = try JSONDecoder().decode(RestoreResult.self, from: result) }
                    else {
                        state = try JSONDecoder().decode(BrowserState.self, from: result)
                        selection = nil; search = ""
                    }
                } catch { self.error = error.localizedDescription }
            }
        } catch { self.error = error.localizedDescription; busy = false }
    }
    func chooseSource() {
        let panel = NSOpenPanel()
        panel.title = "Choose a backup or disk image"; panel.prompt = "Open Backup"
        panel.message = "Open a ZFS send, an offline pool image, or a raw, QCOW2 or VMDK disk image."
        panel.canChooseDirectories = false; panel.allowsMultipleSelection = false
        if panel.runModal() == .OK, let url = panel.url { open(url) }
    }
    func open(_ url: URL) { perform(["method": "open", "path": url.path], activity: "Opening backup…") }
    func unlockFile() {
        let panel = NSOpenPanel(); panel.title = "Choose the encryption key"; panel.prompt = "Unlock"
        panel.message = "Choose the key file for this dataset. Slide keys may be 32 raw bytes or 64 hexadecimal characters."
        if panel.runModal() == .OK, let url = panel.url {
            perform(["method": "unlock", "key_file": url.path], activity: "Unlocking backup…")
        }
    }
    func unlock() {
        let key = secret; secret = ""; showUnlock = false
        perform(["method": "unlock", "key": key], activity: "Unlocking backup…")
    }
    func browse(_ entry: Entry) {
        if entry.directory {
            let path = state?.path ?? "/"
            perform(["method": "list", "path": (path == "/" ? "" : path) + "/" + entry.name], activity: "Opening folder…")
        } else if entry.image { enter(entry) }
    }
    func enter(_ entry: Entry) { perform(["method": "enter", "name": entry.name], activity: "Finding filesystems…") }
    func up() {
        guard let path = state?.path else { return }
        perform(["method": "list", "path": (path as NSString).deletingLastPathComponent], activity: "Opening parent folder…")
    }
    func restore() {
        guard let entry = selected else { return }
        let panel = NSSavePanel(); panel.title = entry.directory ? "Restore folder" : "Restore file"
        panel.prompt = "Restore"; panel.nameFieldStringValue = entry.name
        panel.message = "Choose where to save the recovered \(entry.directory ? "folder" : "file"). Existing files are kept."
        panel.canCreateDirectories = true
        if panel.runModal() == .OK, let url = panel.url {
            perform(["method": "extract", "name": entry.name, "destination": url.path], activity: "Restoring \(entry.name)…", restore: true)
        }
    }
}

struct ContentView: View {
    @ObservedObject var model: Recovery
    @AppStorage("snapshotTimeZone") private var timeZone = "local"
    @State private var timeZoneRevision = 0
    var body: some View {
        // Read this state so system time-zone notifications refresh visible dates.
        let _ = timeZoneRevision
        HStack(spacing: 0) {
            sidebar.frame(width: 270)
            Divider()
            VStack(spacing: 0) {
                if let state = model.state, !state.title.isEmpty { browser(state) } else { welcome }
                Divider()
                HStack(spacing: 9) {
                    if model.busy { ProgressView().controlSize(.small); Text(model.activity) }
                    else { Image(systemName: "lock.shield").foregroundStyle(.green); Text("Read-only source · No ZFS installation needed") }
                    Spacer()
                    if let state = model.state, !state.locked { Text("\(model.entries.count) \(model.entries.count == 1 ? "item" : "items")").foregroundStyle(.secondary) }
                }.font(.system(size: 12)).padding(.horizontal, 22).frame(height: 38)
            }
        }
        .frame(minWidth: 1000, minHeight: 660)
        .toolbar {
            ToolbarItemGroup(placement: .navigation) {
                Button { model.chooseSource() } label: { Label("Open Backup", systemImage: "folder.badge.plus") }
                    .keyboardShortcut("o").help("Choose a backup or disk image")
                Button { model.showPath = true } label: { Label("Open Path", systemImage: "text.cursor") }
                    .help("Open a pasted file or device path")
            }
            ToolbarItem(placement: .primaryAction) {
                Button { model.restore() } label: { Label("Restore…", systemImage: "arrow.down.to.line") }
                    .keyboardShortcut("s").disabled(model.selected == nil || !(model.selected!.regular || model.selected!.directory))
            }
        }
        .disabled(model.busy)
        .overlay(alignment: .top) {
            if let error = model.error {
                VStack(alignment: .leading, spacing: 8) {
                    HStack { Label("Couldn’t complete that step", systemImage: "exclamationmark.triangle.fill").font(.headline); Spacer(); Button("Dismiss") { model.error = nil } }
                    ScrollView { Text(error).textSelection(.enabled).frame(maxWidth: .infinity, alignment: .leading) }.frame(maxHeight: 140)
                }.padding(18).background(.regularMaterial, in: RoundedRectangle(cornerRadius: 12))
                    .overlay(RoundedRectangle(cornerRadius: 12).stroke(.orange.opacity(0.5))).padding(20).shadow(radius: 8)
            }
        }
        .sheet(isPresented: $model.showUnlock, onDismiss: { model.secret = "" }) {
            VStack(alignment: .leading, spacing: 18) {
                Label("Unlock this backup", systemImage: "lock.open").font(.title2.bold())
                Text(model.state?.views[model.state?.view ?? 0].key_format == "raw" ? "Enter the 64-character Slide key, or choose a raw key file from the previous screen." : "Enter the passphrase or hexadecimal key for this dataset.").foregroundStyle(.secondary)
                SecureField("Encryption key or passphrase", text: $model.secret).textFieldStyle(.roundedBorder).onSubmit { if !model.secret.isEmpty { model.unlock() } }
                Text("Used only for this session. Never saved to settings or recovery reports.").font(.caption).foregroundStyle(.secondary)
                HStack { Spacer(); Button("Cancel") { model.secret = ""; model.showUnlock = false }; Button("Unlock") { model.unlock() }.keyboardShortcut(.defaultAction).disabled(model.secret.isEmpty) }
            }.padding(28).frame(width: 450)
        }
        .sheet(isPresented: $model.showPath) {
            VStack(alignment: .leading, spacing: 18) {
                Text("Open a backup path").font(.title2.bold())
                Text("Paste the full path to a backup, offline disk image, or readable device.").foregroundStyle(.secondary)
                TextField("/Volumes/Backups/slide.img", text: $model.sourcePath).textFieldStyle(.roundedBorder)
                HStack { Spacer(); Button("Cancel") { model.showPath = false }; Button("Open") { model.showPath = false; model.open(URL(fileURLWithPath: (model.sourcePath as NSString).expandingTildeInPath)) }.keyboardShortcut(.defaultAction).disabled(model.sourcePath.isEmpty) }
            }.padding(28).frame(width: 480)
        }
        .onOpenURL { model.open($0) }
        .sheet(isPresented: $model.showPreferences) {
            TimeZonePreferences(selection: $timeZone)
        }
        .onReceive(NotificationCenter.default.publisher(for: .NSSystemTimeZoneDidChange)) { _ in
            timeZoneRevision += 1
        }
    }
    var sidebar: some View {
        VStack(alignment: .leading, spacing: 22) {
            HStack(spacing: 10) {
                Image(systemName: "externaldrive.badge.timemachine").font(.system(size: 29)).foregroundStyle(.teal)
                VStack(alignment: .leading) { Text("ZFS Explore").font(.title3.bold()); Text("File recovery").font(.caption).foregroundStyle(.secondary) }
            }.padding(.top, 18)
            if let state = model.state, !state.title.isEmpty {
                VStack(alignment: .leading, spacing: 6) {
                    Text("BACKUP").font(.caption.bold()).foregroundStyle(.secondary)
                    Text(state.title).font(.headline).lineLimit(3).textSelection(.enabled)
                    if let summary = state.summary { Text(summary).font(.caption).foregroundStyle(.secondary) }
                }
                if !state.views.isEmpty {
                    VStack(alignment: .leading, spacing: 8) {
                        Text("SNAPSHOTS & DATASETS").font(.caption.bold()).foregroundStyle(.secondary)
                        Button {
                            model.showPreferences = true
                        } label: {
                            Label("Times: \(SnapshotTime.zoneLabel(timeZone))", systemImage: "globe")
                                .font(.caption).lineLimit(2)
                        }.buttonStyle(.plain).foregroundStyle(.secondary)
                            .help("Change the time zone used for snapshot dates")
                        ScrollView {
                            VStack(spacing: 4) {
                                ForEach(Array(state.views.enumerated()), id: \.offset) { i, item in
                                    Button {
                                        model.perform(["method": "select", "index": i], activity: "Opening snapshot…")
                                    } label: {
                                        HStack(alignment: .top, spacing: 8) {
                                            Image(systemName: item.encrypted ? "lock" : "clock").frame(width: 16)
                                            VStack(alignment: .leading, spacing: 5) {
                                                if let date = SnapshotTime.label(item.created_at, zone: timeZone) {
                                                    Text(date).font(.system(size: 12, weight: .medium)).fixedSize(horizontal: false, vertical: true)
                                                    Text(item.label).font(.system(size: 10)).foregroundStyle(.secondary).lineLimit(2)
                                                } else {
                                                    Text(item.label).font(.system(size: 12)).lineLimit(4)
                                                }
                                            }.frame(maxWidth: .infinity, alignment: .leading)
                                        }.padding(10).background(state.view == i ? Color.accentColor.opacity(0.13) : .clear, in: RoundedRectangle(cornerRadius: 7))
                                    }.buttonStyle(.plain).help(item.label)
                                }
                            }
                        }
                    }
                }
                if !state.layers.isEmpty {
                    VStack(alignment: .leading, spacing: 7) {
                        Text("INSIDE DISK IMAGE").font(.caption.bold()).foregroundStyle(.secondary)
                        ForEach(Array(state.layers.enumerated()), id: \.offset) { _, layer in Text((layer as NSString).lastPathComponent).font(.caption).lineLimit(2) }
                        if state.can_back { Button("Back to outer backup") { model.perform(["method": "back"], activity: "Returning to backup…") } }
                    }
                }
                Spacer(minLength: 0)
                Button("Close Backup") { model.perform(["method": "close"], activity: "Closing backup…") }
            } else {
                VStack(alignment: .leading, spacing: 22) {
                    step("1", "Choose your backup", "ZFS send or offline disk image")
                    step("2", "Find your files", "Select a snapshot, then browse")
                    step("3", "Restore a copy", "Save files to a new destination")
                }.padding(.top, 20)
                Spacer()
            }
            Label("Your backup stays unchanged", systemImage: "checkmark.shield").font(.caption).foregroundStyle(.secondary)
        }.padding(.horizontal, 20).padding(.bottom, 20).background(Color(nsColor: .controlBackgroundColor))
    }
    func step(_ number: String, _ title: String, _ detail: String) -> some View {
        HStack(alignment: .top, spacing: 10) {
            Text(number).font(.caption.bold()).frame(width: 23, height: 23).background(.teal.opacity(0.12), in: Circle())
            VStack(alignment: .leading, spacing: 4) { Text(title).font(.subheadline.bold()); Text(detail).font(.caption).foregroundStyle(.secondary) }
        }
    }
    var welcome: some View {
        VStack(spacing: 20) {
            Spacer()
            Image(systemName: "externaldrive.fill").font(.system(size: 66, weight: .light)).foregroundStyle(.teal)
            Text("Get your files back.").font(.system(size: 32, weight: .semibold))
            Text("Open a backup, find the file you need, and save a copy.\nBrowse encrypted Slide backups without mounting a disk.").multilineTextAlignment(.center).foregroundStyle(.secondary).lineSpacing(4)
            Button("Choose Backup…") { model.chooseSource() }.buttonStyle(.borderedProminent).controlSize(.large)
            Text("ZFS send · ZFS pool image · RAW · QCOW2 · VMDK").font(.caption).foregroundStyle(.secondary)
            Spacer()
        }.frame(maxWidth: .infinity).padding(40)
    }
    func browser(_ state: BrowserState) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack {
                VStack(alignment: .leading, spacing: 5) {
                    Text(state.layers.isEmpty ? "Browse backup" : "Browse disk image").font(.title2.bold())
                    Text(state.path.isEmpty ? "/" : state.path).font(.system(.subheadline, design: .monospaced)).foregroundStyle(.secondary).textSelection(.enabled).lineLimit(2)
                }
                Spacer()
                if !state.locked { TextField("Find in this folder", text: $model.search).onChange(of: model.search) { _ in model.selection = nil }.textFieldStyle(.roundedBorder).frame(width: 190) }
            }.padding(22)
            if state.locked {
                VStack(spacing: 18) {
                    Image(systemName: "lock.shield").font(.system(size: 46)).foregroundStyle(.teal)
                    Text("This backup is encrypted").font(.title2.bold())
                    Text("Unlock the selected dataset to browse and recover its files.").foregroundStyle(.secondary)
                    HStack { Button("Enter Key…") { model.showUnlock = true }.buttonStyle(.borderedProminent); Button("Choose Key File…") { model.unlockFile() } }
                    Text("Your key is kept only for this session.").font(.caption).foregroundStyle(.secondary)
                }.frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                HStack {
                    Button { model.up() } label: { Label("Up", systemImage: "arrow.up") }.disabled(state.path == "/")
                    if !state.volumes.isEmpty {
                        Picker("Volume", selection: Binding(get: { state.volume ?? "" }, set: { model.perform(["method": "volume", "selector": $0], activity: "Opening volume…") })) {
                            Text("Choose a volume…").tag("")
                            ForEach(state.volumes) { volume in Text(volume.display).tag(volume.selector).disabled(!volume.supported) }
                        }
                    }
                    Spacer()
                    if let entry = model.selected, entry.image {
                        Button("Explore as Disk Image") { model.enter(entry) }
                    }
                    Button("Restore…") { model.restore() }.buttonStyle(.borderedProminent)
                        .disabled(model.selected == nil || !(model.selected!.regular || model.selected!.directory))
                }.padding(.horizontal, 22).padding(.bottom, 14)
                Table(model.entries, selection: $model.selection) {
                    TableColumn("Name") { entry in
                        HStack { Image(systemName: entry.icon).foregroundStyle(entry.directory ? .teal : .secondary).frame(width: 20); Text(entry.name).lineLimit(1) }
                            .contextMenu { if entry.directory { Button("Open Folder") { model.browse(entry) } }; if entry.regular { Button("Explore as Disk Image") { model.enter(entry) } }; Button("Restore…") { model.selection = entry.name; model.restore() }.disabled(!entry.regular && !entry.directory) }
                    }.width(min: 220, ideal: 330)
                    TableColumn("Kind") { entry in Text(entry.detail).foregroundStyle(.secondary) }.width(85)
                    TableColumn("Size") { entry in Text((entry.directory ? nil : entry.size).map { ByteCountFormatter.string(fromByteCount: Int64(clamping: $0), countStyle: .file) } ?? "—").monospacedDigit().foregroundStyle(.secondary) }.width(90)
                }.contextMenu(forSelectionType: String.self) { _ in } primaryAction: { ids in
                    if let name = ids.first, let entry = state.entries.first(where: { $0.name == name }) { model.browse(entry) }
                }
                .overlay {
                    if model.entries.isEmpty {
                        Text(state.volume == nil && !state.volumes.isEmpty ? "Choose a supported volume above to browse its files." : model.search.isEmpty ? "This folder is empty." : "No files match your search.").foregroundStyle(.secondary).padding()
                    }
                }
                if let result = model.restored {
                    VStack(alignment: .leading, spacing: 8) {
                        HStack { Label("Restore complete", systemImage: "checkmark.circle.fill").font(.headline).foregroundStyle(.green); Spacer(); Button("Show in Finder") { NSWorkspace.shared.activateFileViewerSelecting([URL(fileURLWithPath: result.restored)]) } }
                        Text("\(result.files) \(result.files == 1 ? "file" : "files") · \(ByteCountFormatter.string(fromByteCount: Int64(clamping: result.bytes), countStyle: .file)) saved to \(result.restored)").font(.caption).textSelection(.enabled)
                        if let hash = result.sha256 { Text("SHA-256  \(hash)").font(.system(size: 10, design: .monospaced)).textSelection(.enabled) }
                        if result.skipped > 0 { Text("\(result.skipped) links or special entries were skipped.").font(.caption).foregroundStyle(.orange) }
                    }.padding(18).background(.green.opacity(0.07))
                } else {
                    HStack { Image(systemName: "info.circle"); Text("Double-click folders or disk images to explore. Select a file or folder, then Restore.") }.font(.caption).foregroundStyle(.secondary).padding(16)
                }
            }
        }
    }
}

struct TimeZonePreferences: View {
    @Binding var selection: String
    @Environment(\.dismiss) private var dismiss
    @State private var search = ""
    private var zones: [String] {
        TimeZone.knownTimeZoneIdentifiers.filter {
            $0 != "UTC" && (search.isEmpty || $0.replacingOccurrences(of: "_", with: " ").localizedCaseInsensitiveContains(search))
        }.sorted()
    }
    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("Snapshot time zone").font(.title2.bold())
            Text("Show backup dates in the time zone that makes sense to you. This changes how dates appear; the backup stays unchanged.")
                .foregroundStyle(.secondary)
            choice("local", "Use my Mac’s time zone", detail: TimeZone.autoupdatingCurrent.identifier)
            choice("UTC", "UTC", detail: "Coordinated Universal Time")
            Divider()
            TextField("Find another time zone or city", text: $search).textFieldStyle(.roundedBorder)
            ScrollView {
                VStack(spacing: 2) {
                    ForEach(zones, id: \.self) { zone in
                        choice(zone, zone.replacingOccurrences(of: "_", with: " "))
                    }
                    if zones.isEmpty { Text("No matching time zones").foregroundStyle(.secondary).padding() }
                }
            }.frame(height: 220)
            Text("Selected: \(SnapshotTime.zoneLabel(selection))").font(.caption)
            HStack { Text("Saved automatically for future sessions.").font(.caption).foregroundStyle(.secondary); Spacer(); Button("Done") { dismiss() }.keyboardShortcut(.defaultAction) }
        }.padding(26).frame(width: 510)
    }
    func choice(_ identifier: String, _ title: String, detail: String? = nil) -> some View {
        Button { selection = identifier } label: {
            HStack(spacing: 10) {
                Image(systemName: selection == identifier ? "checkmark.circle.fill" : "circle").foregroundStyle(selection == identifier ? Color.accentColor : .secondary)
                VStack(alignment: .leading, spacing: 3) {
                    Text(title)
                    if let detail { Text(detail).font(.caption).foregroundStyle(.secondary) }
                }
                Spacer()
            }.padding(8).contentShape(Rectangle())
        }.buttonStyle(.plain).accessibilityLabel(title).accessibilityValue(selection == identifier ? "Selected" : "")
    }
}

final class AppDelegate: NSObject, NSApplicationDelegate {
    func applicationDidFinishLaunching(_ notification: Notification) { NSApp.setActivationPolicy(.regular); NSApp.activate(ignoringOtherApps: true) }
    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { true }
}

@main struct ZFSExplore: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) var delegate
    @StateObject private var model = Recovery()
    var body: some Scene {
        Window("ZFS Explore", id: "recovery") { ContentView(model: model).tint(.teal) }
            .defaultSize(width: 1120, height: 740)
            .commands {
                CommandGroup(replacing: .appSettings) { Button("Settings…") { model.showPreferences = true }.keyboardShortcut(",").disabled(model.busy) }
                CommandGroup(replacing: .newItem) { Button("Open Backup…") { model.chooseSource() }.keyboardShortcut("o").disabled(model.busy) }
            }
    }
}
