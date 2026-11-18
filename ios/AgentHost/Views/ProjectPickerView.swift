import SwiftUI

/// Pick a folder for a new project on the computer, or create one, inside the
/// project folders chosen on the desktop (default ~/Code). With a single
/// project folder the picker opens inside it.
struct ProjectPickerView: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.dismiss) private var dismiss
    var onPicked: (WorkspaceInfo) -> Void

    @State private var roots: ProjectBrowseInfo?
    @State private var error: String?

    var body: some View {
        NavigationStack {
            Group {
                if let roots, roots.folders.count == 1, let only = roots.folders.first {
                    ProjectFolderList(path: only.path, isRoot: true, onPicked: picked)
                } else if roots != nil {
                    ProjectFolderList(path: nil, isRoot: true, onPicked: picked)
                } else if let error {
                    ContentUnavailableView {
                        Label("Can't Load Folders", systemImage: "folder.badge.questionmark")
                    } description: {
                        Text(error)
                    } actions: {
                        Button("Try Again") { Task { await load() } }
                    }
                } else {
                    ProgressView()
                }
            }
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button {
                        dismiss()
                    } label: {
                        ToolbarIcon(systemName: "chevron.left")
                    }
                    .accessibilityLabel("Back")
                }
            }
        }
        .presentationDetents([.large])
        .presentationDragIndicator(.visible)
        .task { await load() }
    }

    private func picked(_ w: WorkspaceInfo) {
        onPicked(w)
        dismiss()
    }

    private func load() async {
        do {
            roots = try await model.browseProjects(nil)
            error = nil
        } catch {
            self.error = userMessage(for: error)
        }
    }
}

/// One level: the project folders (`path` nil), or a folder's subfolders.
/// Tap to go in; swipe for Use; the toolbar makes a new folder.
struct ProjectFolderList: View {
    @EnvironmentObject private var model: AppModel
    let path: String?
    /// The first level shown (its back button closes the picker).
    var isRoot = false
    var onPicked: (WorkspaceInfo) -> Void

    @State private var browse: ProjectBrowseInfo?
    @State private var error: String?
    @State private var busy = false
    @State private var askName = false
    @State private var newName = ""
    @State private var search = ""

    /// Inside a project folder (not one of the project folders itself).
    private var canUseThis: Bool { path != nil && browse?.parent != nil }

    private var folders: [ProjectFolderInfo] {
        let all = browse?.folders ?? []
        let q = search.trimmingCharacters(in: .whitespaces)
        return q.isEmpty ? all : all.filter { $0.name.localizedCaseInsensitiveContains(q) }
    }

    var body: some View {
        List {
            if let error {
                Section {
                    Text(error).foregroundStyle(Palette.red)
                }
            }
            if canUseThis, let path {
                Section {
                    Button {
                        Task { await pick { try await model.createProject(path: path) } }
                    } label: {
                        Label("Use “\(title)”", systemImage: "checkmark.circle")
                    }
                    .disabled(busy)
                }
            }
            Section {
                if let b = browse, b.folders.isEmpty {
                    Text(path == nil
                         ? "No project folders. Add one on the computer under Settings → Workspaces."
                         : "No folders yet. Tap \(Image(systemName: "folder.badge.plus")) to create one.")
                        .foregroundStyle(.secondary)
                }
                ForEach(folders) { f in
                    NavigationLink {
                        ProjectFolderList(path: f.path, onPicked: onPicked)
                    } label: {
                        HStack(spacing: 12) {
                            Image(systemName: "folder.fill")
                                .foregroundStyle(Color.accentColor)
                                .frame(width: 26)
                            VStack(alignment: .leading, spacing: 2) {
                                Text(f.name)
                                    .lineLimit(1)
                                    .truncationMode(.middle)
                                if f.workspaceId != nil {
                                    Text("Already a project")
                                        .font(.footnote)
                                        .foregroundStyle(.secondary)
                                }
                            }
                            .alignmentGuide(.listRowSeparatorLeading) { $0[.leading] }
                        }
                        .padding(.vertical, 2)
                    }
                    .swipeActions(edge: .trailing, allowsFullSwipe: true) {
                        if path != nil {
                            Button("Use") {
                                Task { await pick { try await model.createProject(path: f.path) } }
                            }
                            .tint(.accentColor)
                        }
                    }
                }
            } footer: {
                if path == nil {
                    Text("Projects can only be made inside these folders on your computer. Change them on the computer.")
                } else if browse?.parent == nil {
                    Text("Tap a folder to look inside, or swipe left to use it. Only folders inside \(title) on your computer are shown; change them on the computer.")
                }
            }
        }
        .listStyle(.insetGrouped)
        .searchable(text: $search, placement: .navigationBarDrawer(displayMode: .always), prompt: "Search folders")
        .navigationBarTitleDisplayMode(.inline)
        .navTitle(title, subtitle: "on \(model.hostName)")
        .navigationBarBackButtonHidden(isRoot)
        .toolbar {
            if path != nil {
                ToolbarItem(placement: .primaryAction) {
                    Button {
                        newName = ""
                        askName = true
                    } label: {
                        ToolbarIcon(systemName: "folder.badge.plus")
                    }
                    .disabled(busy)
                    .accessibilityLabel("New Folder")
                }
            }
        }
        .alert("New Project Folder", isPresented: $askName) {
            TextField("Name", text: $newName)
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
            Button("Cancel", role: .cancel) {}
            Button("Create") {
                let name = newName.trimmingCharacters(in: .whitespacesAndNewlines)
                guard let path, !name.isEmpty else { return }
                Task { await pick { try await model.createProject(parent: path, name: name) } }
            }
        } message: {
            Text("Creates the folder in \(path.map(abbreviatedPath) ?? "") on \(model.hostName) and starts a project there.")
        }
        .overlay {
            if browse == nil && error == nil || busy {
                ProgressView()
            }
        }
        .task {
            await load()
            #if DEBUG
            if model.isDemo, DemoData.requestedScreen == .picker, isRoot {
                newName = "weather-cli"
                askName = true
            }
            #endif
        }
        .refreshable { await load() }
    }

    private var title: String {
        guard let path else { return "Project Folders" }
        return URL(fileURLWithPath: path).lastPathComponent
    }

    private func load() async {
        do {
            browse = try await model.browseProjects(path)
            error = nil
        } catch {
            self.error = userMessage(for: error)
        }
    }

    private func pick(_ op: @escaping () async throws -> WorkspaceInfo) async {
        busy = true
        defer { busy = false }
        do {
            onPicked(try await op())
        } catch {
            self.error = userMessage(for: error)
        }
    }
}
