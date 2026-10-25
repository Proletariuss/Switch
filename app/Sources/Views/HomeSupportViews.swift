import SwiftUI

struct ToastBubble: View {
    let toast: ToastItem
    let onClose: () -> Void

    var body: some View {
        HStack(alignment: .top, spacing: 8) {
            Image(systemName: toast.icon ?? (toast.isSuccess ? "checkmark.circle.fill" : "xmark.circle.fill"))
                .foregroundStyle(toast.isSuccess ? .green : .red)
            VStack(alignment: .leading, spacing: 2) {
                Text(toast.title ?? (toast.isSuccess ? "检查完成" : "检查失败"))
                    .font(.callout.bold())
                Text(toast.message)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .textSelection(.enabled)
            }
            Spacer(minLength: 8)
            Button {
                onClose()
            } label: {
                Image(systemName: "xmark")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            .buttonStyle(.plain)
            .help("关闭")
        }
        .padding(12)
        .frame(maxWidth: 360, alignment: .leading)
        .glassCard()
        .shadow(color: .black.opacity(0.15), radius: 8, y: 2)
    }
}

struct ServiceRowView: View {
    let index: Int
    let service: NetworkService
    let isDragging: Bool

    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: "line.3.horizontal")
                .font(.caption)
                .foregroundStyle(.tertiary)
            Text("\(index + 1)")
                .font(.callout.monospacedDigit())
                .foregroundStyle(.secondary)
                .frame(width: 18, alignment: .leading)
            Text(service.name)
                .font(.callout)
                .lineLimit(1)
                .truncationMode(.middle)
            badge
            Spacer()
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 4)
        .background(
            RoundedRectangle(cornerRadius: 6)
                .fill(isDragging ? Color.accentColor.opacity(0.12) : Color.clear))
        .contentShape(Rectangle())
    }

    @ViewBuilder
    private var badge: some View {
        if service.module == true {
            Text("模块网卡")
                .font(.caption2.bold())
                .padding(.horizontal, 5).padding(.vertical, 1)
                .background(Capsule().fill(Color.green.opacity(0.18)))
                .foregroundStyle(.green)
        } else if service.usb == true {
            Text("USB")
                .font(.caption2.bold())
                .padding(.horizontal, 5).padding(.vertical, 1)
                .background(Capsule().fill(Color.accentColor.opacity(0.15)))
                .foregroundStyle(Color.accentColor)
        }
    }
}

struct ServiceDropDelegate: DropDelegate {
    let destination: Int
    @Binding var dragged: Int?
    let onMove: (Int, Int) -> Void

    func dropEntered(info: DropInfo) {
        guard let from = dragged, from != destination else { return }
        withAnimation(ProletariusMotion.quick) {
            onMove(from, destination)
        }
        dragged = destination
    }

    func dropUpdated(info: DropInfo) -> DropProposal? {
        DropProposal(operation: .move)
    }

    func performDrop(info: DropInfo) -> Bool {
        dragged = nil
        return true
    }
}
