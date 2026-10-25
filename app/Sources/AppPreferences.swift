import AppKit
import SwiftUI

enum AppPreferenceKeys {
    static let appearance = "appAppearance"
    static let privacyMaskEnabled = "privacyMaskEnabled"
}

enum AppAppearance: String, CaseIterable, Identifiable {
    case system
    case light
    case dark

    var id: String { rawValue }

    var title: String {
        switch self {
        case .system: return "跟随系统"
        case .light: return "浅色"
        case .dark: return "深色"
        }
    }

    @MainActor
    static func apply(_ rawValue: String) {
        let appearance: NSAppearance?
        switch AppAppearance(rawValue: rawValue) ?? .system {
        case .system: appearance = nil
        case .light: appearance = NSAppearance(named: .aqua)
        case .dark: appearance = NSAppearance(named: .darkAqua)
        }

        // 清除窗口级缓存后再切换应用外观，避免“深色 → 跟随系统”残留局部深色材质。
        for window in NSApp.windows {
            window.appearance = nil
        }
        NSApp.appearance = appearance
        for window in NSApp.windows {
            window.contentView?.needsLayout = true
            window.contentView?.needsDisplay = true
        }
    }
}

enum PrivacyMask {
    static func value(_ value: String, enabled: Bool) -> String {
        guard enabled else { return value }
        let trimmed = value.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty,
              trimmed != "-",
              !trimmed.contains("未知"),
              !trimmed.contains("不可用") else {
            return value
        }

        let characters = Array(value)
        guard characters.count > 4 else { return String(repeating: "•", count: max(4, characters.count)) }
        let prefixCount = characters.count > 10 ? 3 : 2
        let suffixCount = 2
        let hiddenCount = min(10, max(4, characters.count - prefixCount - suffixCount))
        return String(characters.prefix(prefixCount))
            + String(repeating: "•", count: hiddenCount)
            + String(characters.suffix(suffixCount))
    }
}
