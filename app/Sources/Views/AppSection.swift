import SwiftUI

/// Navigation sections and their presentation metadata.
///
/// Keeping this value type separate from ContentView prevents the root view
/// from becoming the owner of unrelated navigation styling and makes the
/// section list reusable by the menu-bar and state summary views.
enum AppSection: String, CaseIterable, Identifiable {
    case activity = "活动"
    case home = "模块"
    case reader = "读卡器"
    case multistandby = "N待"
    case sms = "短信中心"
    case calls = "通话"
    case debug = "调试与诊断"

    var id: String { rawValue }

    var accent: Color {
        switch self {
        case .activity: return ProletariusPalette.activity
        case .home: return ProletariusPalette.module
        case .reader: return ProletariusPalette.reader
        case .multistandby: return ProletariusPalette.reader
        case .sms: return ProletariusPalette.messages
        case .calls: return ProletariusPalette.calls
        case .debug: return ProletariusPalette.diagnostics
        }
    }

    var subtitle: String {
        switch self {
        case .activity: return "运行状态与实时活动"
        case .home: return "DJI 模块与蜂窝状态"
        case .reader: return "PC/SC、eSIM 与 Wi-Fi Calling"
        case .multistandby: return "多号码在线管理"
        case .sms: return "所有线路短信集中收发"
        case .calls: return "蜂窝语音工作台"
        case .debug: return "运行证据与诊断工具"
        }
    }

    var icon: String {
        switch self {
        case .activity: return "waveform.path.ecg.rectangle"
        case .home: return "antenna.radiowaves.left.and.right"
        case .reader: return "creditcard.and.123"
        case .multistandby: return "Mobius"
        case .sms: return "bubble.left.and.bubble.right"
        case .calls: return "phone"
        case .debug: return "stethoscope"
        }
    }
}
