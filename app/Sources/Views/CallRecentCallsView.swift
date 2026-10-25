import SwiftUI

struct CallRecentCallsView: View {
    @Environment(\.workspaceViewport) private var viewport
    let records: [CallRecord]
    let privacyMaskEnabled: Bool
    let canRedial: (CallRecord) -> Bool
    let redialHelp: (CallRecord) -> String
    let onRedial: (CallRecord) -> Void
    let onDelete: (CallRecord) -> Void
    let onShowAll: () -> Void

    private var visibleRecords: [CallRecord] {
        Array(records.prefix(5))
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(alignment: .firstTextBaseline) {
                Text("最近通话")
                    .font(.system(size: 14, weight: .semibold))
                Spacer()
                Button("全部 \(records.count)", action: onShowAll)
                    .controlSize(.small)
                    .disabled(records.isEmpty)
            }
            .padding(.bottom, viewport.isCompactHeight ? 8 : 12)

            if records.isEmpty {
                emptyState
            } else {
                VStack(spacing: 0) {
                    ForEach(visibleRecords) { record in
                        row(record)
                        if record.id != visibleRecords.last?.id {
                            Divider()
                        }
                    }
                }
            }
        }
    }

    private var emptyState: some View {
        VStack(spacing: 7) {
            WorkspaceSymbol(systemName: "clock.arrow.circlepath", tint: WorkspaceColors.accent, size: 40)
            Text("暂无通话记录")
                .font(.callout.weight(.medium))
            Text("完成的呼叫会显示在这里")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
        .frame(maxWidth: .infinity, minHeight: viewport.isCompactHeight ? 130 : 190)
    }

    private func row(_ record: CallRecord) -> some View {
        let number = record.number?.trimmingCharacters(in: .whitespacesAndNewlines)

        return HStack(spacing: 10) {
            Image(systemName: icon(record))
                .foregroundStyle(color(record))
                .frame(width: 26, height: 32)
                .accessibilityHidden(true)

            VStack(alignment: .leading, spacing: 2) {
                Text(PrivacyMask.value(number?.isEmpty == false ? number! : "未知号码", enabled: privacyMaskEnabled))
                    .font(.system(size: 13, weight: .medium))
                    .lineLimit(1)
                    .truncationMode(.middle)
                    .copyableInformation(number ?? "", enabled: !privacyMaskEnabled)
                Text(metadata(record))
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(2)
            }

            Spacer(minLength: 8)

            Button {
                onRedial(record)
            } label: {
                Image(systemName: "phone.fill")
                    .frame(width: 18, height: 18)
            }
            .buttonStyle(.plain)
            .foregroundStyle(canRedial(record) ? WorkspaceColors.accent : Color.secondary)
            .disabled(!canRedial(record))
            .help(redialHelp(record))
            .accessibilityLabel(number.map { "回拨 \(PrivacyMask.value($0, enabled: privacyMaskEnabled))" } ?? "号码不可用")
        }
        .padding(.vertical, viewport.isCompactHeight ? 7 : 11)
        .contentShape(Rectangle())
        .contextMenu {
            if canRedial(record), let number, !number.isEmpty {
                Button {
                    onRedial(record)
                } label: {
                    Label("拨打 \(PrivacyMask.value(number, enabled: privacyMaskEnabled))", systemImage: "phone.fill")
                }
            }
            Button(role: .destructive) {
                onDelete(record)
            } label: {
                Label("删除这条记录", systemImage: "trash")
            }
        }
        .accessibilityElement(children: .contain)
    }

    private func icon(_ record: CallRecord) -> String {
        if record.isMissed { return "phone.arrow.down.left.fill" }
        return record.isIncoming ? "phone.arrow.down.left" : "phone.arrow.up.right"
    }

    private func color(_ record: CallRecord) -> Color {
        if record.isMissed { return .red }
        return record.isIncoming ? .green : WorkspaceColors.accent
    }

    private func kind(_ record: CallRecord) -> String {
        if record.isMissed { return "未接" }
        return record.isIncoming ? "呼入" : "呼出"
    }

    private func metadata(_ record: CallRecord) -> String {
        let date: String
        if Calendar.current.isDateInToday(record.startedAt) {
            date = "今天 \(record.startedAt.formatted(date: .omitted, time: .shortened))"
        } else if Calendar.current.isDateInYesterday(record.startedAt) {
            date = "昨天 \(record.startedAt.formatted(date: .omitted, time: .shortened))"
        } else {
            date = record.startedAt.formatted(date: .abbreviated, time: .shortened)
        }

        guard record.duration > 0 else { return "\(kind(record)) · \(record.deviceName) · \(date)" }
        return "\(kind(record)) · \(record.deviceName) · \(date) · \(durationText(record.duration))"
    }

    private func durationText(_ duration: Int) -> String {
        let minutes = duration / 60
        let seconds = duration % 60
        return minutes > 0 ? "\(minutes)分\(seconds)秒" : "\(seconds)秒"
    }
}
