import SwiftUI

/// Pick a folder for a new project on the computer, or create one, inside the
/// project folders chosen on the desktop (default ~/Code).
struct ProjectPickerView: View {
    @Environment(\.dismiss) private var dismiss
    var onPicked: (WorkspaceInfo) -> Void

    var body: some View {
        NavigationStack {
            ProjectFolderList(path: nil) { w in
                onPicked(w)
                dismiss()
            }
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
            }
        }
    }
}

/// One level: the project folders (`path` nil), or a folder's subfolders.
struct ProjectFolderList: View {
    @EnvironmentObject private var model: AppModel
    let path: String?
    var onPicked: (WorkspaceInfo) -> Void

    @State private var browse: ProjectBrowseInfo?
    @State private var error: String?
    @State private var busy = false
    @State private var askName = false
    @State private var newName = ""

    /// Inside a project folder (not one of the project folders itself).
    private var canUse: Bool { path != nil && browse?.parent != nil }

    var body: some View {
        List {
            if let error {
                Section {
                    Text(error).foregroundStyle(.red)
                }
            }
            if canUse, let path {
                Section {
                    Button {
                        Task { await pick { try await model.createProject(path: path) } }
                    } label: {
                        Label("Use This Folder", systemImage: "checkmark.circle")
                    }
                    .disabled(busy)
                }
            }
            Section {
                if let b = browse, b.folders.isEmpty {
                    Text(path == nil
                         ? "No project folders. Add one on the desktop under Workspaces."
                         : "No folders yet. Tap \(Image(systemName: "folder.badge.plus")) to create one.")
                        .foregroundStyle(.secondary)
                }
                ForEach(browse?.folders ?? []) { f in
                    NavigationLink {
                        ProjectFolderList(path: f.path, onPicked: onPicked)
                    } label: {
                        HStack {
                            Label(f.name, systemImage: path == nil ? "folder.fill" : "folder")
                                .lineLimit(1)
                                .truncationMode(.middle)
                            Spacer()
                            if f.workspaceId != nil {
                                Text("Project")
                                    .font(.caption)
                                    .foregroundStyle(.secondary)
                            }
                        }
                    }
                }
            } footer: {
                if path == nil {
                    Text("Projects can only be made inside these folders on your computer. Change them on the desktop.")
                }
            }
        }
        .navigationTitle(title)
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            if path != nil {
                ToolbarItem(placement: .primaryAction) {
                    Button {
                        newName = ""
                        askName = true
                    } label: {
                        Image(systemName: "folder.badge.plus")
                    }
                    .disabled(busy)
                    .accessibilityLabel("New Folder")
                }
            }
        }
        .alert("New Folder", isPresented: $askName) {
            TextField("Name", text: $newName)
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
            Button("Create") {
                let name = newName.trimmingCharacters(in: .whitespacesAndNewlines)
                guard let path, !name.isEmpty else { return }
                Task { await pick { try await model.createProject(parent: path, name: name) } }
            }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("Creates the folder on your computer and starts a project there.")
        }
        .overlay {
            if browse == nil && error == nil || busy {
                ProgressView()
            }
        }
        .task { await load() }
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
