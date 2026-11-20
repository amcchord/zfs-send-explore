import AppKit
import Combine
import SwiftUI
import UniformTypeIdentifiers

struct BackupView: Decodable {
    let label: String; let encrypted: Bool; let key_format: String?
    let created_at: UInt64?; let selector: String
    let kind: String?; let name: String?; let snapshot: String?; let parent: Bool?; let detail: String?
    var isCurrent: Bool { kind == "current" }
    /// A dated or captured copy that holds files, as opposed to a live dataset
    /// head or a parent dataset that only contains other backups.
    var isRestorePoint: Bool { !isCurrent && !(parent ?? false) }
}
struct Entry: Decodable, Identifiable {
    let name: String; let directory: Bool; let regular: Bool; let size: UInt64?
    var id: String { name }
    var image: Bool { regular && ["raw", "img", "vhd", "vmdk", "qcow2", "datto"].contains(URL(fileURLWithPath: name).pathExtension.lowercased()) }
    var restorable: Bool { regular || directory }
    var icon: String { directory ? "folder.fill" : image ? "externaldrive.fill" : regular ? "doc" : "link" }
    var detail: String { directory ? "Folder" : image ? "Disk image" : regular ? "File" : "Link or special item" }
    var sizeLabel: String { directory || !regular ? "—" : Sizes.label(size ?? 0) }
}
struct Volume: Decodable, Identifiable {
    let selector: String; let label: String; let supported: Bool; let bytes: UInt64; let filesystem: String?
    let title: String?; let purpose: String?; let partition: UInt32?; let recommended: Bool?
    var id: String { selector }
    /// "Partition 3 · NTFS · 41.9 GB" — where it is and how big, without repeating its purpose.
    var placement: String {
        let place = partition.map { $0 == 0 ? "Whole disk" : "Partition \($0)" } ?? selector
        return [place, filesystem ?? "unreadable format", Sizes.label(bytes)].joined(separator: " · ")
    }
}
struct DatasetChoice: Decodable, Identifiable {
    let name: String; let index: Int
    var id: String { name }
}
struct FileVersion: Decodable, Identifiable {
    let index: Int; let snapshot: String; let created_at: UInt64?; let size: UInt64?; let issue: String?
    var id: Int { index }
}
struct FileHistory: Decodable, Identifiable {
    let name: String; let versions: [FileVersion]
    var id: String { name }
}
struct BrowserState: Decodable {
    let title: String; let summary: String?; let description: String?; let views: [BackupView]; let view: Int
    let locked: Bool; let path: String; let entries: [Entry]; let volumes: [Volume]
    let volume: String?; let layers: [String]; let can_back: Bool
    let choose_dataset: Bool?; let dataset_choices: [DatasetChoice]?; let recommended_view: Int?
    var selectedView: BackupView? { choose_dataset != true && views.indices.contains(view) ? views[view] : nil }
    var needsVolume: Bool { !layers.isEmpty && volume == nil && !volumes.isEmpty }
}
struct RestoreResult: Decodable { let restored: String; let bytes: UInt64; let files: UInt64; let skipped: UInt64; let sha256: String?; let source: String?; let source_path: String?; let report: String? }
struct ServiceError: LocalizedError {
    var title = "That didn’t work"
    let message: String
    var details: String? = nil
    var errorDescription: String? { message }
}

extension Color {
    /// Raised surface for cards in both appearances.
    static let card = Color(nsColor: .controlBackgroundColor)
}

enum Sizes {
    static func label(_ bytes: UInt64) -> String {
        let formatter = ByteCountFormatter()
        formatter.countStyle = .file
        formatter.allowsNonnumericFormatting = false
        return formatter.string(fromByteCount: Int64(clamping: bytes))
    }
}

struct JobProgress: Decodable, Sendable {
    let read_bytes: UInt64
    let written_bytes: UInt64
    let files: UInt64
}

/// Serial private pipe to the shared Rust engine. Blocking work never runs on the main actor.
final class Engine: @unchecked Sendable {
    private let queue = DispatchQueue(label: "tech.zfs-explore.engine", qos: .userInitiated)
    private var process: Process?
    private var input: FileHandle?
    private var output: FileHandle?
    private var pending = Data()
    // The read queue blocks until completion. Cancellation has its own writer
    // lock so it can reach a running operation without waiting for its reply.
    private let writeLock = NSLock()
    private var runningJob: String?
    private var cancelRequested: String?

    func cancel(_ job: String) {
        writeLock.lock()
        defer { writeLock.unlock() }
        cancelRequested = job
        if runningJob == job { sendCancel(job) }
    }
    private func sendCancel(_ job: String) {
        if let data = try? JSONSerialization.data(withJSONObject: ["method":"cancel", "job":job]) {
            try? input?.write(contentsOf: data + Data([10]))
        }
    }
    private func start(_ data: Data, job: String) throws {
        writeLock.lock()
        defer { writeLock.unlock() }
        try input?.write(contentsOf: data + Data([10]))
        runningJob = job
        if cancelRequested == job { sendCancel(job) }
    }
    private func finish() {
        writeLock.lock()
        runningJob = nil
        cancelRequested = nil
        writeLock.unlock()
    }
    func request(_ data: Data, job: String, report: @escaping @Sendable (JobProgress) -> Void) async throws -> Data {
        try await withCheckedThrowingContinuation { continuation in
            queue.async {
                defer { self.finish() }
                do {
                    if self.process == nil {
                        let process = Process()
                        guard let binary = Bundle.main.url(forAuxiliaryExecutable: "zfs-explore-service") else {
                            throw ServiceError(title: "ZFS Explore is incomplete", message: "The recovery engine is missing. Reinstall the complete app from the disk image.")
                        }
                        process.executableURL = binary
                        let input = Pipe(), output = Pipe()
                        process.standardInput = input; process.standardOutput = output
                        process.standardError = FileHandle.nullDevice
                        try process.run()
                        self.process = process; self.input = input.fileHandleForWriting; self.output = output.fileHandleForReading
                    }
                    try self.start(data, job: job)
                    while true {
                        while !self.pending.contains(10) {
                            guard let chunk = self.output?.availableData, !chunk.isEmpty else {
                                throw ServiceError(title: "The recovery engine stopped", message: "Quit and reopen ZFS Explore to try again. Your backup has not been changed.")
                            }
                            self.pending.append(chunk)
                        }
                        let end = self.pending.firstIndex(of: 10)!
                        let response = self.pending.prefix(upTo: end)
                        self.pending.removeSubrange(...end)
                        guard let envelope = try JSONSerialization.jsonObject(with: response) as? [String: Any] else {
                            throw ServiceError(message: "The recovery engine returned an invalid response.")
                        }
                        if let event = envelope["progress"] {
                            if envelope["job"] as? String == job {
                                let data = try JSONSerialization.data(withJSONObject: event)
                                report(try JSONDecoder().decode(JobProgress.self, from: data))
                            }
                            continue
                        }
                        if envelope["ok"] as? Bool != true {
                            let raw = envelope["error"] as? String ?? "The operation failed."
                            throw ServiceError(title: envelope["title"] as? String ?? "That didn’t work",
                                               message: envelope["message"] as? String ?? raw, details: raw)
                        }
                        continuation.resume(returning: try JSONSerialization.data(withJSONObject: envelope["result"]!))
                        break
                    }
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
    @Published var progress: JobProgress?
    @Published var cancellationRequested = false
    private var activeJob: String?
    func cancelOperation() {
        guard let job = activeJob else { return }
        cancellationRequested = true
        engine.cancel(job)
    }
    @Published var error: ServiceError?
    @Published var history: FileHistory?
    @Published var restored: RestoreResult?
    @Published var search = ""
    @Published var secret = ""
    @Published var showPath = false
    @Published var showDrives = false
    @Published var drives: [AttachedDrive] = []
    @Published var driveSelection = DriveSelection()
    @Published var showPreferences = false
    @Published var sourcePath = ""
    /// Name of the drive chosen in the picker, shown instead of its device node.
    @Published var driveTitle: (path: String, title: String)?
    private let engine = Engine()
    /// The open backup's name: a drive's model, or the file name.
    var title: String {
        guard let state else { return "" }
        if let drive = driveTitle, (drive.path as NSString).lastPathComponent == state.title { return drive.title }
        return state.title
    }
    var isOpen: Bool { state.map { !$0.title.isEmpty } ?? false }
    var selected: Entry? { state?.entries.first { $0.name == selection } }
    var entries: [Entry] { state?.entries.filter { search.isEmpty || $0.name.localizedCaseInsensitiveContains(search) } ?? [] }
    var canRestore: Bool { selected?.restorable ?? false }
    /// Disk images in the current folder, largest first, when the user has not yet opened one.
    var suggestedImages: [Entry] {
        guard let state, !state.locked, state.layers.isEmpty else { return [] }
        return state.entries.filter(\.image).sorted { ($0.size ?? 0) > ($1.size ?? 0) }
    }

    func performRequest(_ request: [String: Any]) async throws -> Data {
        let job = UUID().uuidString
        activeJob = job
        progress = nil
        cancellationRequested = false
        defer { activeJob = nil }
        var message = request
        message["job"] = job
        return try await engine.request(try JSONSerialization.data(withJSONObject: message), job: job) { [weak self] progress in
            Task { @MainActor in
                guard self?.activeJob == job else { return }
                self?.progress = progress
            }
        }
    }

    @discardableResult
    func run(_ request: [String: Any], activity: String, restore: Bool = false) async -> Bool {
        guard !busy else { return false }
        busy = true; self.activity = activity; error = nil
        restored = nil
        defer { busy = false }
        do {
            let result = try await performRequest(request)
            if restore { restored = try JSONDecoder().decode(RestoreResult.self, from: result) }
            else {
                state = try JSONDecoder().decode(BrowserState.self, from: result)
                selection = nil; search = ""
            }
            return true
        } catch let failure as ServiceError {
            error = failure
        } catch {
            self.error = ServiceError(message: error.localizedDescription)
        }
        return false
    }
    func perform(_ request: [String: Any], activity: String, restore: Bool = false) {
        Task { await run(request, activity: activity, restore: restore) }
    }

    func chooseSource() {
        let panel = NSOpenPanel()
        panel.title = "Open a Backup"; panel.prompt = "Open"
        panel.message = "Choose a backup file or disk image. For a striped ZFS pool, select all its member images together."
        panel.canChooseDirectories = false; panel.allowsMultipleSelection = true
        if panel.runModal() == .OK { open(panel.urls) }
    }
    func open(_ url: URL) { open([url]) }
    func open(_ urls: [URL], drive: (path: String, title: String)? = nil) {
        guard let first = urls.first else { return }
        let device = urls.contains { $0.path.hasPrefix("/dev/") }
        let activity = device ? "Requesting read access to the selected disks…" :
            (urls.count == 1 ? "Opening \(first.lastPathComponent)…" : "Opening \(urls.count) pool members…")
        Task {
            if await run(["method": "open", "paths": urls.map(\.path), "authorize": device], activity: activity) {
                driveTitle = drive
            }
        }
    }
    func openDropped(_ providers: [NSItemProvider]) {
        Task {
            var urls: [URL] = []
            for provider in providers {
                let url: URL? = await withCheckedContinuation { continuation in
                    _ = provider.loadObject(ofClass: URL.self) { url, _ in continuation.resume(returning: url) }
                }
                guard let url, url.isFileURL else {
                    error = ServiceError(message: "Choose the backup files again using Open.")
                    return
                }
                urls.append(url)
            }
            open(urls)
        }
    }
    func chooseDrive() {
        driveSelection = DriveSelection()
        refreshDrives()
        showDrives = true
    }
    func refreshDrives() {
        let current = AttachedDrive.scan()
        if current != drives { drives = current }
    }
    func openDrive() {
        refreshDrives()
        let selected = driveSelection.available(in: drives)
        guard !selected.isEmpty, selected.count == driveSelection.count else { return }
        let (drive, slice) = selected[0]
        let title = slice.whole ? drive.name : "\(drive.name) · \(slice.typeLabel)"
        showDrives = false
        open(selected.map { URL(fileURLWithPath: $0.1.path) },
             drive: selected.count == 1 ? (slice.path, title) : nil)
    }
    func openPath() {
        showPath = false
        let paths = sourcePath.components(separatedBy: .newlines)
            .map { $0.trimmingCharacters(in: .whitespaces) }.filter { !$0.isEmpty }
        open(paths.map { URL(fileURLWithPath: ($0 as NSString).expandingTildeInPath) })
    }
    func select(_ index: Int) {
        guard state?.choose_dataset == true || index != state?.view else { return }
        secret = ""
        perform(["method": "select", "index": index], activity: "Opening restore point…")
    }
    func findVersions(_ name: String) {
        guard !busy else { return }
        busy = true; activity = "Checking restore points…"; error = nil; restored = nil
        Task {
            defer { busy = false }
            do {
                let response = try await performRequest(["method": "versions", "name": name])
                history = try JSONDecoder().decode(FileHistory.self, from: response)
            } catch let failure as ServiceError { error = failure }
            catch { self.error = ServiceError(message: error.localizedDescription) }
        }
    }
    func openVersion(_ point: FileVersion, name: String) {
        history = nil
        Task {
            if await run(["method": "open_version", "index": point.index, "name": name], activity: "Opening file version…") {
                selection = name
            }
        }
    }

    func unlockFile() {
        let panel = NSOpenPanel(); panel.title = "Choose the Key File"; panel.prompt = "Unlock"
        panel.message = "Choose the encryption key file for this backup. Slide key files are 32 bytes or 64 characters long."
        if panel.runModal() == .OK, let url = panel.url {
            perform(["method": "unlock", "key_file": url.path], activity: "Unlocking…")
        }
    }
    func unlock() {
        guard !secret.isEmpty else { return }
        let key = secret; secret = ""
        perform(["method": "unlock", "key": key], activity: "Unlocking…")
    }
    func open(_ entry: Entry) {
        if entry.directory { list(child(entry.name)) }
        else if entry.image { enter(entry) }
        else { selection = entry.name; restore() }
    }
    func child(_ name: String) -> String {
        let path = state?.path ?? "/"
        return (path == "/" ? "" : path) + "/" + name
    }
    func list(_ path: String) { perform(["method": "list", "path": path], activity: "Opening folder…") }
    func enter(_ entry: Entry) { perform(["method": "enter", "name": entry.name], activity: "Looking inside \(entry.name)…") }
    func chooseVolume(_ volume: Volume) { perform(["method": "volume", "selector": volume.selector], activity: "Opening \(volume.purpose ?? "partition")…") }
    func up() {
        guard let path = state?.path, path != "/" else { return }
        list((path as NSString).deletingLastPathComponent)
    }
    /// Leave nested disk images until `depth` layers remain open.
    func leave(toDepth depth: Int) {
        Task {
            while let state, state.layers.count > depth, state.can_back {
                guard await run(["method": "back"], activity: "Returning…") else { return }
            }
        }
    }
    func close() { perform(["method": "close"], activity: "Closing backup…"); restored = nil }
    func restore() {
        guard let entry = selected, entry.restorable else { return }
        restored = nil; error = nil
        let panel = NSSavePanel(); panel.title = entry.directory ? "Restore Folder" : "Restore File"
        panel.prompt = "Restore"; panel.nameFieldStringValue = entry.name
        panel.message = "Choose where to save a copy of “\(entry.name)”. Existing files are never replaced."
        panel.canCreateDirectories = true
        panel.directoryURL = FileManager.default.urls(for: .desktopDirectory, in: .userDomainMask).first
        if panel.runModal() == .OK, let url = panel.url {
            perform(["method": "extract", "name": entry.name, "destination": url.path], activity: "Restoring \(entry.name)…", restore: true)
        }
    }
}

// MARK: - Window

struct ContentView: View {
    @ObservedObject var model: Recovery
    @AppStorage("snapshotTimeZone") private var timeZone = "local"
    @State private var timeZoneRevision = 0
    @State private var columns = NavigationSplitViewVisibility.detailOnly
    @State private var dropTargeted = false
    var body: some View {
        // Read this state so system time-zone notifications refresh visible dates.
        let _ = timeZoneRevision
        NavigationSplitView(columnVisibility: $columns) {
            Sidebar(model: model, timeZone: timeZone)
                .disabled(model.busy)
                .navigationSplitViewColumnWidth(min: 250, ideal: 280, max: 360)
        } detail: {
            Detail(model: model, timeZone: timeZone)
                .navigationSplitViewColumnWidth(min: 620, ideal: 820)
        }
        .frame(minWidth: 920, minHeight: 620)
        .navigationTitle("ZFS Explore")
        .navigationSubtitle(model.isOpen ? model.title : "")
        .toolbar {
            ToolbarItem(placement: .navigation) {
                Menu {
                    Button("Open Backup File…") { model.chooseSource() }
                    Button("Open Connected Drive…") { model.chooseDrive() }
                    Button("Open from a Path…") { model.showPath = true }
                    if model.isOpen { Divider(); Button("Close Backup") { model.close() } }
                } label: { Label("Open", systemImage: "folder.badge.plus").labelStyle(.titleAndIcon) }
                .disabled(model.busy)
                .help("Open a backup file or a connected backup drive")
            }
            if model.isOpen {
                ToolbarItem(placement: .primaryAction) {
                    Button { model.chooseDrive() } label: {
                        Label("Connected Drives", systemImage: "externaldrive").labelStyle(.titleAndIcon)
                    }
                    .disabled(model.busy)
                    .help("Choose connected backup drives (⇧⌘D)")
                }
            }
            ToolbarItem(placement: .primaryAction) {
                Button { model.restore() } label: { Label("Restore…", systemImage: "square.and.arrow.down").labelStyle(.titleAndIcon) }
                    .disabled(model.busy || !model.canRestore)
                    .help("Save a copy of the selected file or folder")
            }
        }
        .onChange(of: model.isOpen) { _, open in withAnimation { columns = open ? .all : .detailOnly } }
        .sheet(isPresented: $model.showPath) { OpenPathSheet(model: model).tint(.teal) }
        .sheet(isPresented: $model.showDrives) { DrivePicker(model: model).tint(.teal) }
        .sheet(item: $model.history) { history in
            VStack(alignment: .leading, spacing: 12) {
                Text("Versions of “\(history.name)”").font(.title2.bold())
                Text("Select a restore point to open this file.").foregroundStyle(.secondary)
                List(history.versions) { point in
                    Button { model.openVersion(point, name: history.name) } label: {
                        HStack {
                            VStack(alignment: .leading, spacing: 3) {
                                Text(SnapshotTime.short(point.created_at, zone: timeZone) ?? "Date unavailable").bold()
                                Text(point.snapshot).font(.caption)
                                if let issue = point.issue { Text(issue).font(.caption).foregroundStyle(.secondary) }
                            }
                            Spacer()
                            if let size = point.size { Text(Sizes.label(size)).monospacedDigit() }
                            if point.issue == nil { Image(systemName: "chevron.right") }
                        }.contentShape(Rectangle())
                    }.buttonStyle(.plain).disabled(point.issue != nil)
                }
                HStack { Spacer(); Button("Close") { model.history = nil }.keyboardShortcut(.cancelAction) }
            }.padding(20).frame(minWidth: 500, minHeight: 360)
        }
        .sheet(isPresented: $model.showPreferences) { TimeZonePreferences(selection: $timeZone).tint(.teal) }
        .onOpenURL { model.open($0) }
        .onDrop(of: [.fileURL], isTargeted: $dropTargeted) { providers in
            guard !providers.isEmpty else { return false }
            model.openDropped(providers)
            return true
        }
        .overlay {
            if dropTargeted {
                RoundedRectangle(cornerRadius: 14).strokeBorder(Color.teal, style: StrokeStyle(lineWidth: 3, dash: [10, 6]))
                    .background(Color.teal.opacity(0.06), in: RoundedRectangle(cornerRadius: 14)).padding(10)
                    .overlay(Label("Drop to open this backup", systemImage: "arrow.down.doc").font(.title2.bold()).foregroundStyle(.teal))
                    .allowsHitTesting(false)
            }
        }
        .onReceive(NotificationCenter.default.publisher(for: .NSSystemTimeZoneDidChange)) { _ in
            timeZoneRevision += 1
        }
    }
}

// MARK: - Sidebar

struct Sidebar: View {
    @ObservedObject var model: Recovery
    let timeZone: String
    @State private var showOthers = false
    var body: some View {
        Group {
            if let state = model.state, model.isOpen {
                List(selection: Binding(get: { state.choose_dataset == true ? nil : state.view }, set: { if let index = $0 { model.select(index) } })) {
                    Section {
                        BackupSummary(state: state, title: model.title)
                    }
                    let points = state.views.indices.filter { state.views[$0].isRestorePoint }
                    if !points.isEmpty {
                        Section("Restore points") {
                            ForEach(points, id: \.self) { index in
                                RestorePointRow(view: state.views[index], timeZone: timeZone, newest: state.recommended_view == index && points.count > 1 && state.views[index].created_at != nil)
                                    .tag(index)
                            }
                        }
                    }
                    let others = state.views.indices.filter { !state.views[$0].isRestorePoint }
                    if !others.isEmpty {
                        Section("Other places in this backup (\(others.count))", isExpanded: $showOthers) {
                            ForEach(others, id: \.self) { index in
                                RestorePointRow(view: state.views[index], timeZone: timeZone, newest: false).tag(index)
                            }
                        }
                    }
                }
                .listStyle(.sidebar)
                .onAppear { showOthers = state.views.indices.contains(state.view) && !state.views[state.view].isRestorePoint }
            } else {
                List {
                    Section("How it works") {
                        HowItWorksRow(number: 1, title: "Open your backup", detail: "A backup file or a connected drive")
                        HowItWorksRow(number: 2, title: "Pick a restore point", detail: "The date you want files from")
                        HowItWorksRow(number: 3, title: "Restore your files", detail: "Save copies anywhere you like")
                    }
                }.listStyle(.sidebar)
            }
        }
        .safeAreaInset(edge: .bottom) {
            VStack(alignment: .leading, spacing: 8) {
                if model.isOpen {
                    Button { model.showPreferences = true } label: {
                        Label("Times in \(SnapshotTime.zoneName(timeZone))", systemImage: "globe").lineLimit(1)
                    }.buttonStyle(.link).font(.caption).help("Change the time zone used for restore point dates")
                }
                Label("Read-only: your backup is never changed", systemImage: "checkmark.shield.fill")
                    .font(.caption).foregroundStyle(.secondary)
            }.frame(maxWidth: .infinity, alignment: .leading).padding(.horizontal, 16).padding(.vertical, 12)
        }
    }
}

struct BackupSummary: View {
    let state: BrowserState
    let title: String
    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: "externaldrive.fill.badge.timemachine").font(.system(size: 22)).foregroundStyle(.teal).frame(width: 30)
            VStack(alignment: .leading, spacing: 2) {
                Text(title).font(.headline).lineLimit(2).truncationMode(.middle)
                Text(state.description ?? state.summary ?? "").font(.caption).foregroundStyle(.secondary).lineLimit(2)
            }
        }.padding(.vertical, 4).selectionDisabled()
    }
}

struct RestorePointRow: View {
    let view: BackupView
    let timeZone: String
    let newest: Bool
    var body: some View {
        HStack(alignment: .top, spacing: 9) {
            Image(systemName: view.encrypted ? "lock.fill" : view.isCurrent ? "circle.dashed" : view.parent == true ? "folder" : "clock.arrow.circlepath")
                .foregroundStyle(.teal).frame(width: 16).padding(.top, 2)
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 6) {
                    Text(title).fontWeight(.medium).lineLimit(1)
                    if newest {
                        Text("Newest").font(.caption2.bold()).foregroundStyle(.teal)
                            .padding(.horizontal, 5).padding(.vertical, 1).background(.teal.opacity(0.14), in: Capsule())
                    }
                }
                Text(subtitle).font(.caption).foregroundStyle(.secondary).lineLimit(1)
            }
        }
        .padding(.vertical, 2)
        .help([SnapshotTime.label(view.created_at, zone: timeZone), view.label].compactMap { $0 }.joined(separator: "\n"))
    }
    var title: String {
        if let date = SnapshotTime.short(view.created_at, zone: timeZone) { return date }
        if view.isCurrent { return "Current files" }
        if view.parent == true { return view.name ?? view.label }
        return view.snapshot ?? view.name ?? view.label
    }
    var subtitle: String {
        if view.parent == true { return "Holds other backups" }
        return view.detail ?? view.label
    }
}

struct HowItWorksRow: View {
    let number: Int; let title: String; let detail: String
    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            Text("\(number)").font(.caption.bold()).foregroundStyle(.teal)
                .frame(width: 22, height: 22).background(.teal.opacity(0.14), in: Circle())
            VStack(alignment: .leading, spacing: 2) {
                Text(title).fontWeight(.medium)
                Text(detail).font(.caption).foregroundStyle(.secondary)
            }
        }.padding(.vertical, 3)
    }
}

// MARK: - Detail

struct Detail: View {
    @ObservedObject var model: Recovery
    let timeZone: String
    @State private var showBusy = false
    var body: some View {
        VStack(spacing: 0) {
            if let state = model.state, model.isOpen {
                StepBar(model: model, state: state, timeZone: timeZone)
                Divider()
                if state.choose_dataset == true {
                    VStack(alignment: .leading, spacing: 14) {
                        Text("Choose a computer or dataset").font(.title2.bold())
                        Text("This backup contains several sets of files. Choose one to see its latest restore point.")
                        List(state.dataset_choices ?? []) { choice in
                            Button(choice.name) { model.select(choice.index) }
                        }
                    }.padding(24)
                } else if state.locked { UnlockView(model: model, view: state.selectedView) }
                else if state.needsVolume { VolumeChooser(model: model, state: state) }
                else { BrowserView(model: model, state: state) }
            } else {
                WelcomeView(model: model)
            }
        }
        .disabled(model.busy)
        .overlay(alignment: .top) {
            if let error = model.error { ProblemBanner(error: error) { model.error = nil }.padding(16) }
        }
        .overlay {
            if showBusy {
                VStack(spacing: 12) {
                    ProgressView().controlSize(.large)
                    Text(model.activity).font(.headline)
                    if let progress = model.progress {
                        Text("\(Sizes.label(progress.read_bytes)) read · \(Sizes.label(progress.written_bytes)) written · \(progress.files) files finished")
                            .font(.caption).monospacedDigit().foregroundStyle(.secondary)
                    }
                    Button(model.cancellationRequested ? "Cancelling…" : "Cancel") { model.cancelOperation() }
                        .disabled(model.cancellationRequested)
                }
                .padding(28).frame(minWidth: 260)
                .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 16))
                .shadow(radius: 12, y: 4)
                .transition(.opacity)
            }
        }
        .onChange(of: model.busy) { _, busy in
            if busy {
                Task { try? await Task.sleep(nanoseconds: 350_000_000); if model.busy { withAnimation { showBusy = true } } }
            } else { withAnimation { showBusy = false } }
        }
    }
}

/// Where the user is in the recovery: open → restore point → unlock → find → restore.
struct StepBar: View {
    @ObservedObject var model: Recovery
    let state: BrowserState
    let timeZone: String
    var body: some View {
        // Narrow windows drop the detail lines before anything is clipped.
        ViewThatFits(in: .horizontal) {
            row(details: true, titles: true)
            row(details: false, titles: true)
            row(details: false, titles: false)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.horizontal, 20).padding(.vertical, 12)
    }
    var steps: [(index: Int, title: String, detail: String)] {
        let restored = model.restored != nil
        var steps = [
            (0, "Backup open", state.description?.components(separatedBy: " · ").first ?? "Ready"),
            (1, "Restore point", pointLabel),
        ]
        if state.selectedView?.encrypted ?? false {
            steps.append((2, state.locked ? "Unlock" : "Unlocked", state.locked ? "Enter the key" : "Key accepted"))
        }
        steps.append((3, "Find files", state.layers.isEmpty ? "Browse folders" : "Inside a disk image"))
        steps.append((4, restored ? "Restored" : "Restore", restored ? "Copy saved" : "Save a copy"))
        return steps
    }
    var current: Int { state.locked ? 2 : model.restored != nil ? 4 : 3 }
    func row(details: Bool, titles: Bool) -> some View {
        HStack(spacing: 0) {
            ForEach(Array(steps.enumerated()), id: \.offset) { offset, step in
                if offset > 0 { connector }
                self.step(step.index, titles || step.index == current ? step.title : nil, details ? step.detail : nil)
            }
        }
    }
    var pointLabel: String {
        guard let view = state.selectedView else { return "—" }
        return SnapshotTime.compact(view.created_at, zone: timeZone) ?? (view.isCurrent ? "Current files" : view.snapshot ?? view.name ?? view.label)
    }
    var connector: some View { Rectangle().fill(.quaternary).frame(height: 1).frame(minWidth: 10, maxWidth: 40).padding(.horizontal, 6) }
    func step(_ index: Int, _ title: String?, _ detail: String?) -> some View {
        let done = index < current || (index == 4 && model.restored != nil)
        let active = index == current && !done
        return HStack(spacing: 8) {
            ZStack {
                Circle().fill(done ? Color.teal : active ? Color.teal.opacity(0.16) : Color.secondary.opacity(0.12))
                if done { Image(systemName: "checkmark").font(.system(size: 10, weight: .bold)).foregroundStyle(.white) }
                else { Circle().fill(active ? Color.teal : Color.secondary.opacity(0.45)).frame(width: 7, height: 7) }
            }.frame(width: 20, height: 20)
            if let title {
                VStack(alignment: .leading, spacing: 1) {
                    Text(title).font(.caption.weight(.semibold)).foregroundStyle(active || done ? .primary : .secondary)
                    if let detail { Text(detail).font(.caption2).foregroundStyle(.secondary) }
                }
                .lineLimit(1).fixedSize()
            }
        }
        .accessibilityElement(children: .combine)
        .accessibilityValue(done ? "Done" : active ? "Current step" : "Not started")
    }
}

struct WelcomeView: View {
    @ObservedObject var model: Recovery
    var body: some View {
        ScrollView {
            VStack(spacing: 26) {
                Spacer(minLength: 30)
                Image(nsImage: NSApp.applicationIconImage).resizable().frame(width: 96, height: 96)
                VStack(spacing: 10) {
                    Text("Open your backup").font(.system(size: 34, weight: .bold))
                    Text("Browse restore points and save copies of your files. Your backup opens read-only.")
                        .multilineTextAlignment(.center).foregroundStyle(.secondary).lineSpacing(3)
                }
                HStack(spacing: 16) {
                    ChoiceCard(icon: "externaldrive", title: "Connected drives", detail: "Choose an attached backup drive, including Slide or Datto drives that Finder can’t show.", shortcut: "⇧⌘D", prominent: true) { model.chooseDrive() }
                    ChoiceCard(icon: "doc.badge.clock", title: "Open a backup file", detail: "Browse a ZFS send file, a pool image or a virtual disk.", shortcut: "⌘O", prominent: false) { model.chooseSource() }
                }.frame(maxWidth: 680)
                HStack(spacing: 6) {
                    Button("Open from a path…") { model.showPath = true }.buttonStyle(.link)
                    Text("·").foregroundStyle(.tertiary)
                    Text("or drag a backup file into this window").foregroundStyle(.secondary)
                }.font(.callout)
            }.frame(maxWidth: .infinity).padding(.horizontal, 40)
        }
    }
}

struct ChoiceCard: View {
    let icon: String; let title: String; let detail: String; let shortcut: String; let prominent: Bool
    let action: () -> Void
    @State private var hover = false
    var body: some View {
        Button(action: action) {
            VStack(alignment: .leading, spacing: 12) {
                Image(systemName: icon).font(.system(size: 24, weight: .medium))
                    .foregroundStyle(prominent ? .white : .teal)
                    .frame(width: 48, height: 48)
                    .background(prominent ? AnyShapeStyle(Color.teal) : AnyShapeStyle(Color.teal.opacity(0.14)), in: RoundedRectangle(cornerRadius: 12))
                VStack(alignment: .leading, spacing: 5) {
                    HStack { Text(title).font(.title3.weight(.semibold)); Spacer(); Text(shortcut).font(.caption).foregroundStyle(.tertiary) }
                    Text(detail).font(.callout).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                }
            }
            .padding(20).frame(maxWidth: .infinity, minHeight: 170, alignment: .topLeading)
            .background(RoundedRectangle(cornerRadius: 16).fill(Color.card))
            .overlay(RoundedRectangle(cornerRadius: 16).strokeBorder(hover || prominent ? Color.teal.opacity(hover ? 0.9 : 0.45) : Color.secondary.opacity(0.2), lineWidth: hover ? 2 : 1))
            .contentShape(RoundedRectangle(cornerRadius: 16))
        }
        .buttonStyle(.plain)
        .onHover { hover = $0 }
        .accessibilityHint(detail)
    }
}

struct UnlockView: View {
    @ObservedObject var model: Recovery
    let view: BackupView?
    @FocusState private var focused: Bool
    var body: some View {
        VStack(spacing: 18) {
            Spacer()
            Image(systemName: "lock.fill").font(.system(size: 30, weight: .semibold)).foregroundStyle(.white)
                .frame(width: 72, height: 72).background(Color.teal, in: Circle())
            Text("This restore point is encrypted").font(.title.bold())
            Text(prompt).multilineTextAlignment(.center).foregroundStyle(.secondary).frame(maxWidth: 480)
            HStack(spacing: 10) {
                SecureField(placeholder, text: $model.secret).textFieldStyle(.roundedBorder).frame(width: 340)
                    .focused($focused).onSubmit { model.unlock() }
                Button("Unlock") { model.unlock() }.buttonStyle(.borderedProminent).keyboardShortcut(.defaultAction).disabled(model.secret.isEmpty)
            }.controlSize(.large)
            Button("Choose Key File…") { model.unlockFile() }
            Label("Your key is used only while ZFS Explore is open and is never saved.", systemImage: "hand.raised.fill")
                .font(.caption).foregroundStyle(.secondary).padding(.top, 6)
            Spacer()
        }.padding(40).frame(maxWidth: .infinity, maxHeight: .infinity)
        .onAppear { focused = true }
    }
    var placeholder: String { view?.key_format == "passphrase" ? "Passphrase" : "Encryption key" }
    var prompt: String {
        switch view?.key_format {
        case "raw": return "Paste the 64-character encryption key from your Slide console, or choose the key file you downloaded."
        case "hex": return "Enter the 64-character encryption key for this backup, or choose a file that contains it."
        case "passphrase": return "Enter the passphrase for this backup, or choose a file that contains it."
        default: return "Enter the encryption key or passphrase for this backup, or choose its key file."
        }
    }
}

struct VolumeChooser: View {
    @ObservedObject var model: Recovery
    let state: BrowserState
    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                BreadcrumbBar(model: model, state: state).padding(.bottom, 4)
                Text("Choose where to look").font(.title2.bold())
                Text("This disk image has \(state.volumes.count) parts. The recommended one usually holds the computer’s documents, programs and settings.")
                    .foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                VStack(spacing: 10) {
                    ForEach(state.volumes) { volume in
                        Button { model.chooseVolume(volume) } label: { VolumeRow(volume: volume) }
                            .buttonStyle(.plain).disabled(!volume.supported)
                    }
                }
            }.padding(24).frame(maxWidth: 720, alignment: .leading).frame(maxWidth: .infinity)
        }
    }
}

struct VolumeRow: View {
    let volume: Volume
    @State private var hover = false
    var body: some View {
        let recommended = volume.recommended == true
        HStack(spacing: 14) {
            Image(systemName: volume.supported ? "internaldrive.fill" : "internaldrive")
                .font(.title2).foregroundStyle(volume.supported ? Color.teal : Color.secondary).frame(width: 34)
            VStack(alignment: .leading, spacing: 3) {
                HStack(spacing: 8) {
                    Text(volume.purpose ?? volume.label).font(.headline)
                    if recommended {
                        Text("Recommended").font(.caption.bold()).foregroundStyle(.white)
                            .padding(.horizontal, 7).padding(.vertical, 2).background(Color.teal, in: Capsule())
                    }
                }
                Text(volume.placement).font(.callout).foregroundStyle(.secondary)
                if !volume.supported { Text("ZFS Explore can’t read this part.").font(.caption).foregroundStyle(.secondary) }
            }
            Spacer()
            if volume.supported { Image(systemName: "chevron.right").foregroundStyle(.tertiary) }
        }
        .padding(16)
        .background(RoundedRectangle(cornerRadius: 12).fill(Color.card))
        .overlay(RoundedRectangle(cornerRadius: 12).strokeBorder(recommended || hover ? Color.teal.opacity(hover ? 0.9 : 0.5) : Color.secondary.opacity(0.18), lineWidth: hover ? 2 : 1))
        .opacity(volume.supported ? 1 : 0.6)
        .contentShape(Rectangle())
        .onHover { hover = $0 && volume.supported }
    }
}

/// Clickable location: outer backup › disk images › folders.
struct BreadcrumbBar: View {
    @ObservedObject var model: Recovery
    let state: BrowserState
    var body: some View {
        let hasSource = !state.views.isEmpty
        let layerNames = state.layers.map { ($0 as NSString).lastPathComponent }
        let parts = state.path.split(separator: "/").map(String.init)
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 4) {
                if hasSource {
                    crumb(model.title, icon: "externaldrive.fill.badge.timemachine", current: layerNames.isEmpty && parts.isEmpty) {
                        if layerNames.isEmpty { model.list("/") } else { model.leave(toDepth: 0) }
                    }
                }
                ForEach(Array(layerNames.enumerated()), id: \.offset) { index, name in
                    if hasSource || index > 0 { separator }
                    crumb(name, icon: "externaldrive.fill", current: index == layerNames.count - 1 && parts.isEmpty) {
                        if index == layerNames.count - 1 { model.list("/") } else { model.leave(toDepth: index + 1) }
                    }
                }
                ForEach(Array(parts.enumerated()), id: \.offset) { index, name in
                    separator
                    crumb(name, icon: "folder.fill", current: index == parts.count - 1) {
                        model.list("/" + parts[...index].joined(separator: "/"))
                    }
                }
            }
        }
    }
    var separator: some View { Image(systemName: "chevron.right").font(.caption2.weight(.semibold)).foregroundStyle(.tertiary) }
    func crumb(_ title: String, icon: String, current: Bool, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Label(title, systemImage: icon).lineLimit(1).truncationMode(.middle)
                .font(.callout.weight(current ? .semibold : .regular))
                .foregroundStyle(current ? .primary : .secondary)
                .padding(.horizontal, 8).padding(.vertical, 4)
                .background(current ? Color.secondary.opacity(0.12) : .clear, in: RoundedRectangle(cornerRadius: 6))
                .frame(maxWidth: 240)
        }.buttonStyle(.plain).disabled(current)
    }
}

struct BrowserView: View {
    @ObservedObject var model: Recovery
    let state: BrowserState
    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 10) {
                Button { model.up() } label: { Image(systemName: "arrow.up").frame(width: 16, height: 16) }
                    .buttonStyle(.bordered).help("Go to the enclosing folder").disabled(state.path == "/")
                BreadcrumbBar(model: model, state: state)
                Spacer(minLength: 8)
                HStack(spacing: 6) {
                    Image(systemName: "magnifyingglass").foregroundStyle(.secondary)
                    TextField("Search this folder", text: $model.search).textFieldStyle(.plain)
                        .onChange(of: model.search) { model.selection = nil }
                    if !model.search.isEmpty { Button { model.search = "" } label: { Image(systemName: "xmark.circle.fill") }.buttonStyle(.plain).foregroundStyle(.secondary) }
                }
                .padding(.horizontal, 8).padding(.vertical, 5)
                .background(RoundedRectangle(cornerRadius: 7).fill(Color.card))
                .overlay(RoundedRectangle(cornerRadius: 7).strokeBorder(.quaternary))
                .frame(width: 210)
            }.padding(.horizontal, 16).padding(.vertical, 10)
            if let image = model.suggestedImages.first {
                ImageCallout(model: model, image: image, count: model.suggestedImages.count).padding(.horizontal, 16).padding(.bottom, 10)
            }
            Table(model.entries, selection: $model.selection) {
                TableColumn("Name") { entry in
                    HStack(spacing: 8) {
                        Image(systemName: entry.icon).foregroundStyle(entry.directory || entry.image ? Color.teal : Color.secondary).frame(width: 18)
                        Text(entry.name).lineLimit(1).truncationMode(.middle)
                    }
                }.width(min: 220, ideal: 380)
                TableColumn("Kind") { entry in Text(entry.detail).foregroundStyle(.secondary) }.width(min: 80, ideal: 110)
                TableColumn("Size") { entry in Text(entry.sizeLabel).monospacedDigit().foregroundStyle(.secondary) }.width(min: 70, ideal: 90)
            }
            .contextMenu(forSelectionType: String.self) { ids in
                if let name = ids.first, let entry = state.entries.first(where: { $0.name == name }) {
                    if entry.directory { Button("Open Folder") { model.open(entry) } }
                    if entry.regular && !state.views.isEmpty { Button("Other Versions…") { model.findVersions(entry.name) } }
                    if entry.regular { Button("Look Inside as a Disk Image") { model.enter(entry) } }
                    Button("Restore…") { model.selection = entry.name; model.restore() }.disabled(!entry.restorable)
                }
            } primaryAction: { ids in
                if let name = ids.first, let entry = state.entries.first(where: { $0.name == name }) { model.open(entry) }
            }
            .overlay {
                if model.entries.isEmpty {
                    VStack(spacing: 8) {
                        Image(systemName: model.search.isEmpty ? "folder" : "magnifyingglass").font(.system(size: 30)).foregroundStyle(.tertiary)
                        Text(model.search.isEmpty ? "This folder is empty" : "No items match “\(model.search)”").font(.headline).foregroundStyle(.secondary)
                    }
                }
            }
            Divider()
            if let result = model.restored { RestoredBanner(model: model, result: result) }
            else { ActionBar(model: model, state: state) }
        }
    }
}

struct ImageCallout: View {
    @ObservedObject var model: Recovery
    let image: Entry
    let count: Int
    var body: some View {
        HStack(spacing: 14) {
            Image(systemName: "externaldrive.fill").font(.title2).foregroundStyle(.teal).frame(width: 34)
            VStack(alignment: .leading, spacing: 3) {
                Text(count == 1 ? "The computer’s files are inside its disk image" : "The computer’s files are inside its disk images").font(.headline)
                Text("Open “\(image.name)” (\(image.sizeLabel)) to browse the computer’s documents and settings.")
                    .font(.callout).foregroundStyle(.secondary).lineLimit(2)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            Button("Open Disk Image") { model.enter(image) }.buttonStyle(.borderedProminent).controlSize(.large)
        }
        .padding(14)
        .frame(height: 88)
        .background(Color.teal.opacity(0.09), in: RoundedRectangle(cornerRadius: 12))
        .overlay(RoundedRectangle(cornerRadius: 12).strokeBorder(Color.teal.opacity(0.3)))
    }
}

struct ActionBar: View {
    @ObservedObject var model: Recovery
    let state: BrowserState
    var body: some View {
        HStack(spacing: 12) {
            if let entry = model.selected {
                Image(systemName: entry.icon).foregroundStyle(.teal).frame(width: 18)
                VStack(alignment: .leading, spacing: 1) {
                    Text(entry.name).fontWeight(.medium).lineLimit(1).truncationMode(.middle)
                    Text(entry.directory ? "Folder · restores everything inside it" : "\(entry.detail) · \(entry.sizeLabel)").font(.caption).foregroundStyle(.secondary)
                }
            } else {
                Image(systemName: "hand.tap").foregroundStyle(.secondary).frame(width: 18)
                Text("Select a file or folder to restore it. Double-click a folder to open it.").foregroundStyle(.secondary)
            }
            Spacer()
            Text("\(model.entries.count) \(model.entries.count == 1 ? "item" : "items")").font(.caption).foregroundStyle(.tertiary)
            if let entry = model.selected, entry.regular && !state.views.isEmpty {
                Button("Other Versions…") { model.findVersions(entry.name) }
            }
            if let entry = model.selected, entry.directory || entry.image {
                Button(entry.directory ? "Open Folder" : "Open Disk Image") { model.open(entry) }.controlSize(.large)
            }
            Button("Restore…") { model.restore() }.buttonStyle(.borderedProminent).controlSize(.large)
                .keyboardShortcut("s").disabled(model.busy || !model.canRestore)
        }.padding(.horizontal, 16).padding(.vertical, 12)
    }
}

struct RestoredBanner: View {
    @ObservedObject var model: Recovery
    let result: RestoreResult
    @State private var details = false
    var body: some View {
        let url = URL(fileURLWithPath: result.restored)
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 12) {
                Image(systemName: "checkmark.circle.fill").font(.title).foregroundStyle(.green)
                VStack(alignment: .leading, spacing: 2) {
                    Text("Restored “\(url.lastPathComponent)”").font(.headline)
                    Text(summary(url)).font(.callout).foregroundStyle(.secondary).lineLimit(1).truncationMode(.middle)
                }
                Spacer()
                Button(details ? "Hide Details" : "Details") { details.toggle() }.buttonStyle(.link)
                Button("Show in Finder") { NSWorkspace.shared.activateFileViewerSelecting([url]) }.buttonStyle(.borderedProminent).controlSize(.large)
                Button { model.restored = nil } label: { Image(systemName: "xmark") }.buttonStyle(.borderless).help("Dismiss")
            }
            if details {
                VStack(alignment: .leading, spacing: 4) {
                    if let source = result.source, let path = result.source_path {
                        Text("From \(source) · \(path)").font(.caption).textSelection(.enabled)
                    }
                    Text(result.restored).font(.caption).textSelection(.enabled)
                    if let report = result.report {
                        Button("Show restore report") { NSWorkspace.shared.activateFileViewerSelecting([URL(fileURLWithPath: report)]) }
                    }
                    if let hash = result.sha256 { Text("SHA-256  \(hash)").font(.system(size: 11, design: .monospaced)).textSelection(.enabled) }
                    Text("Only file contents are restored; owners, permissions and timestamps are not.").font(.caption).foregroundStyle(.secondary)
                }.padding(.leading, 44)
            }
            if result.skipped > 0 {
                Label("\(result.skipped) links or special items were skipped.", systemImage: "info.circle").font(.caption).foregroundStyle(.orange).padding(.leading, 44)
            }
        }
        .padding(.horizontal, 16).padding(.vertical, 12).background(Color.green.opacity(0.08))
    }
    func summary(_ url: URL) -> String {
        let place = url.deletingLastPathComponent().path.replacingOccurrences(of: NSHomeDirectory(), with: "~")
        let files = result.files == 1 ? "1 file" : "\(result.files) files"
        return "\(files) · \(Sizes.label(result.bytes)) · saved in \(place)"
    }
}

struct ProblemBanner: View {
    let error: ServiceError
    let dismiss: () -> Void
    @State private var details = false
    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(alignment: .top, spacing: 12) {
                Image(systemName: "exclamationmark.triangle.fill").font(.title2).foregroundStyle(.orange)
                VStack(alignment: .leading, spacing: 4) {
                    Text(error.title).font(.headline)
                    Text(error.message).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                }
                Spacer()
                if error.details != nil && error.details != error.message {
                    Button(details ? "Hide Details" : "Show Details") { details.toggle() }.buttonStyle(.link)
                }
                Button("OK", action: dismiss).keyboardShortcut(.cancelAction)
            }
            if details, let text = error.details {
                ScrollView { Text(text).font(.system(size: 11, design: .monospaced)).textSelection(.enabled).frame(maxWidth: .infinity, alignment: .leading) }
                    .frame(maxHeight: 130).padding(10).background(Color.card, in: RoundedRectangle(cornerRadius: 8))
            }
        }
        .padding(16).frame(maxWidth: 680)
        .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 14))
        .overlay(RoundedRectangle(cornerRadius: 14).strokeBorder(Color.orange.opacity(0.45)))
        .shadow(color: .black.opacity(0.15), radius: 12, y: 4)
    }
}

struct OpenPathSheet: View {
    @ObservedObject var model: Recovery
    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Label("Open from a path", systemImage: "text.cursor").font(.title2.bold())
            Text("Enter the full path to a backup file or disk device. For a striped pool, enter one member path per line.")
                .foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
            TextEditor(text: $model.sourcePath)
                .font(.body.monospaced()).frame(height: 76).border(.quaternary)
                .accessibilityLabel("Backup paths")
            HStack { Spacer(); Button("Cancel") { model.showPath = false }.keyboardShortcut(.cancelAction); Button("Open") { model.openPath() }.keyboardShortcut(.defaultAction).disabled(model.sourcePath.isEmpty) }
        }.padding(26).frame(width: 500)
    }
}

struct DrivePicker: View {
    @ObservedObject var model: Recovery
    private let refresh = Timer.publish(every: 2, on: .main, in: .common).autoconnect()

    var body: some View {
        let count = model.driveSelection.count
        let unit = model.driveSelection.targets.count == count ? "Drives" : "Members"
        let unavailable = model.driveSelection.unavailable(in: model.drives)
        VStack(alignment: .leading, spacing: 16) {
            Text("Connected drives").font(.title2.bold())
            Text("Select a backup drive. For a striped pool, select each member drive.")
                .foregroundStyle(.secondary)
            if model.drives.isEmpty {
                VStack(spacing: 9) {
                    Image(systemName: "externaldrive.badge.plus")
                        .font(.system(size: 30)).foregroundStyle(.secondary).accessibilityHidden(true)
                    Text("No external drives found").font(.headline)
                    Text("Connect the backup drive and check its power and cable. It will appear here automatically.")
                        .multilineTextAlignment(.center).foregroundStyle(.secondary).frame(maxWidth: 330)
                }.frame(maxWidth: .infinity, minHeight: 218)
            } else {
                ScrollView {
                    VStack(spacing: 0) {
                        ForEach(model.drives) { drive in
                            DriveRow(drive: drive, selection: $model.driveSelection)
                            if drive.id != model.drives.last?.id { Divider().padding(.leading, 44) }
                        }
                    }
                }
                .frame(height: 218)
                .background(.background, in: RoundedRectangle(cornerRadius: 8))
                .overlay(RoundedRectangle(cornerRadius: 8).strokeBorder(.quaternary))
            }
            if !unavailable.isEmpty {
                HStack(alignment: .firstTextBaseline) {
                    let names = unavailable.map(\.bsdName).joined(separator: ", ")
                    Label("Selection unavailable: \(names)", systemImage: "exclamationmark.triangle")
                    Spacer()
                    Button("Remove unavailable") { model.driveSelection.removeUnavailable(in: model.drives) }
                }.font(.callout)
            }
            Label("Drives open read-only. macOS may ask for an administrator password.",
                  systemImage: "lock.shield").foregroundStyle(.secondary).font(.callout)
            HStack {
                Button("Refresh") { model.refreshDrives() }
                Spacer()
                Button("Cancel") { model.showDrives = false }.keyboardShortcut(.cancelAction)
                Button(count > 1 ? "Open \(count) \(unit)" : "Open Drive") { model.openDrive() }
                    .keyboardShortcut(.defaultAction).disabled(count == 0 || !unavailable.isEmpty)
            }
        }
        .padding(24).frame(width: 568)
        .onReceive(refresh) { _ in model.refreshDrives() }
    }
}

private struct DriveRow: View {
    let drive: AttachedDrive
    @Binding var selection: DriveSelection
    @State private var expanded = false
    @State private var preferred: [DriveSlice]?

    private var sources: [DriveSlice] {
        if let chosen = selection.targets[drive.id] { return chosen }
        if let preferred {
            let ids = Set(preferred.map(\.id))
            return drive.slices.filter { ids.contains($0.id) }
        }
        return [drive.suggested]
    }

    private func updateSources(_ slices: [DriveSlice]) {
        preferred = slices
        if selection.targets[drive.id] != nil {
            selection.targets[drive.id] = slices.isEmpty ? nil : slices
        }
    }

    var body: some View {
        let chosen = selection.targets[drive.id]
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 10) {
                Toggle(isOn: Binding(get: { chosen != nil }, set: { checked in
                    selection.targets[drive.id] = checked ? (sources.isEmpty ? [drive.suggested] : sources) : nil
                })) {
                    HStack(spacing: 11) {
                        Image(systemName: "externaldrive").font(.title2).foregroundStyle(.teal)
                            .accessibilityHidden(true)
                        VStack(alignment: .leading, spacing: 3) {
                            Text(drive.name).font(.headline)
                            HStack(spacing: 5) {
                                Text(drive.disk.bsdName)
                                if drive.bus != "Disk image" { Text("·"); Text(drive.bus) }
                                if drive.hasZFSPartition { Text("·"); Text("Likely ZFS").foregroundStyle(.primary) }
                            }.font(.callout).foregroundStyle(.secondary)
                        }
                        Spacer(minLength: 8)
                        Text(drive.disk.sizeLabel).monospacedDigit().foregroundStyle(.secondary)
                    }.contentShape(Rectangle())
                }
                .toggleStyle(.checkbox)
                .accessibilityLabel("\(drive.name), \(drive.disk.bsdName), \(drive.disk.sizeLabel)\(drive.hasZFSPartition ? ", likely ZFS" : "")")
                if !drive.partitions.isEmpty {
                    Button { expanded.toggle() } label: {
                        Image(systemName: expanded ? "chevron.up" : "chevron.down").frame(width: 18, height: 22)
                    }
                    .buttonStyle(.borderless)
                    .accessibilityLabel("\(expanded ? "Hide" : "Show") partitions on \(drive.disk.bsdName)")
                    .help(expanded ? "Hide partitions" : "Show partitions")
                }
            }
            if expanded, !drive.partitions.isEmpty {
                VStack(alignment: .leading, spacing: 7) {
                    if drive.partitions.count == 1 {
                        Picker("Read from", selection: Binding(get: { sources.first?.id ?? drive.suggested.id }, set: { id in
                            if let slice = drive.slices.first(where: { $0.id == id }) { updateSources([slice]) }
                        })) {
                            ForEach(drive.slices) { slice in Text(sourceLabel(slice)).tag(slice.id) }
                        }.accessibilityLabel("Read from \(drive.disk.bsdName)")
                    } else {
                        Text("Read the whole disk or choose member partitions.").font(.caption).foregroundStyle(.secondary)
                        ForEach(drive.slices) { slice in
                            Toggle(sourceLabel(slice), isOn: Binding(get: { sources.contains(where: { $0.id == slice.id }) }, set: { value in
                                updateSources(DriveSelection.changing(sources, slice: slice, checked: value))
                            })).toggleStyle(.checkbox)
                        }
                    }
                }.padding(.leading, 27)
            }
        }.padding(.horizontal, 14).padding(.vertical, 12)
    }

    private func sourceLabel(_ slice: DriveSlice) -> String {
        let label = slice.whole ? "Whole disk" : "\(slice.typeLabel) · \(slice.sizeLabel)"
        let location = slice.mountPath.map { " · mounted at \($0)" } ?? ""
        let hint = slice.id == drive.suggested.id ? " (suggested)" : ""
        return "\(label) · \(slice.bsdName)\(location)\(hint)"
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
            Text("Restore point time zone").font(.title2.bold())
            Text("Choose the time zone used to show when each restore point was made. This only changes how dates appear.")
                .foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
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
            HStack { Text("Selected: \(SnapshotTime.zoneLabel(selection)) · saved automatically").font(.caption).foregroundStyle(.secondary); Spacer(); Button("Done") { dismiss() }.keyboardShortcut(.defaultAction) }
        }.padding(26).frame(width: 510)
    }
    func choice(_ identifier: String, _ title: String, detail: String? = nil) -> some View {
        Button { selection = identifier } label: {
            HStack(spacing: 10) {
                Image(systemName: selection == identifier ? "checkmark.circle.fill" : "circle").foregroundStyle(selection == identifier ? Color.teal : .secondary)
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
            .defaultSize(width: 1180, height: 760)
            .commands {
                CommandGroup(replacing: .appSettings) { Button("Settings…") { model.showPreferences = true }.keyboardShortcut(",").disabled(model.busy) }
                CommandGroup(replacing: .newItem) {
                    Button("Open Backup File…") { model.chooseSource() }.keyboardShortcut("o").disabled(model.busy)
                    Button("Open Connected Drive…") { model.chooseDrive() }.keyboardShortcut("d", modifiers: [.command, .shift]).disabled(model.busy)
                    Button("Open from a Path…") { model.showPath = true }.keyboardShortcut("o", modifiers: [.command, .shift]).disabled(model.busy)
                    Divider()
                    Button("Close Backup") { model.close() }.disabled(model.busy || !model.isOpen)
                }
                CommandGroup(replacing: .help) {
                    Button("ZFS Explore Help") {
                        NSWorkspace.shared.open(URL(string: "https://github.com/amcchord/zfs-send-explore/blob/main/docs/macos-client.md")!)
                    }
                }
            }
    }
}
