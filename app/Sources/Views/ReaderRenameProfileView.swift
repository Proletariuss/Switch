import SwiftUI

struct ReaderRenameProfileView: View {
    @Environment(\.dismiss) private var dismiss
    @Environment(\.workspaceViewport) private var viewport
    let context: ESIMProfileContext
    @ObservedObject var store: ReaderESIMStore

    @State private var name: String
    @State private var busy = false
    @State private var errorMessage: String?

    init(context: ESIMProfileContext, store: ReaderESIMStore) {
        self.context = context
        self.store = store
        _name = State(initialValue: context.profile.name ?? "")
    }

    private var trimmedName: String {
        name.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: viewport.sectionSpacing) {
            HStack(spacing: 12) {
                WorkspaceSymbol(systemName: "pencil.line", tint: WorkspaceColors.accent, size: 42)
                VStack(alignment: .leading, spacing: 4) {
                    Text("修改 Profile 名称").font(.title3.weight(.semibold))
                    Text(context.profile.displayName)
                        .font(.callout)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                }
            }
            VStack(alignment: .leading, spacing: 10) {
                TextField("名称", text: $name)
                    .textFieldStyle(.workspace)
                HStack {
                    Text("最多 64 个字符")
                    Spacer()
                    Text("\(trimmedName.count) / 64").monospacedDigit()
                }
                .font(.caption)
                .foregroundStyle(.secondary)
            }
            if trimmedName.count > 64 {
                Text("当前为 \(trimmedName.count) 个字符，请缩短名称。")
                    .font(.caption)
                    .foregroundStyle(.red)
            }
            if let errorMessage {
                Text(errorMessage).font(.callout).foregroundStyle(.red)
            }
            HStack {
                Button("取消") { dismiss() }
                    .buttonStyle(.proletarius)
                    .keyboardShortcut(.cancelAction)
                    .disabled(busy)
                Spacer()
                Button {
                    save()
                } label: {
                    if busy {
                        ProgressView().controlSize(.small)
                    } else {
                        Text("保存")
                    }
                }
                .buttonStyle(.proletariusPrimary)
                .keyboardShortcut(.defaultAction)
                .disabled(busy || trimmedName.isEmpty || trimmedName.count > 64)
            }
        }
        .padding(viewport.pageInset)
        .frame(width: 430)
        .environment(\.workspaceAccent, WorkspaceColors.accent)
        .environment(\.workspaceVisualStyle, .nekoko)
        .proletariusPageBackground()
        .interactiveDismissDisabled(busy)
    }

    private func save() {
        busy = true
        Task {
            await MainActor.run {
                store.renameProfile(context, newName: trimmedName)
            }
            try? await Task.sleep(nanoseconds: 500_000_000)
            await MainActor.run {
                busy = false
                if store.banner?.tone != .error {
                    dismiss()
                } else {
                    errorMessage = store.banner?.detail ?? "修改失败"
                }
            }
        }
    }
}
