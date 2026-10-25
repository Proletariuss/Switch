import SwiftUI

enum CallLayoutMode: Equatable {
    case wide
    case compact
    case stacked

    static func resolve(width: CGFloat) -> CallLayoutMode {
        if width >= 880 { return .wide }
        if width >= 620 { return .compact }
        return .stacked
    }
}

struct CallLayoutModePreferenceKey: PreferenceKey {
    static let defaultValue = CallLayoutMode.compact

    static func reduce(value: inout CallLayoutMode, nextValue: () -> CallLayoutMode) {
        value = nextValue()
    }
}

enum ReadinessState: Equatable {
    case complete
    case actionRequired
    case current
    case pending
    case failed

    var accessibilityText: String {
        switch self {
        case .complete: return "已完成"
        case .actionRequired: return "待操作"
        case .current: return "当前步骤"
        case .pending: return "等待"
        case .failed: return "失败"
        }
    }
}
