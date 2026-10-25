> 历史记录：以下内容来自 Proletarius，包含当时的功能、路径、发布要求与验证结果，仅供追溯，不是 Switch 的当前指令或验收结论。Switch 已移除传真，当前构建与执行范围以根 README 和桌面工作区改造方案为准。

# 2026-09-28 UI and action audit

## Scope

- Audited the visible SwiftUI buttons and menu actions for empty/no-op handlers.
- Simplified the activity overview and menu-bar panel without removing device, VoWiFi, SMS, call, traffic, update, settings, or quit workflows.
- Replaced implicit `List(selection:)` sidebar routing with explicit button actions that update the selected page.

## Findings and changes

- Empty button closures found by static search were confirmation-dialog cancellation actions; they intentionally dismiss or cancel and were retained.
- Menu-bar SMS notification/adoption toggles duplicated controls already available in the main app and were removed from the menu. System notification settings remain reachable.
- The menu-bar traffic sparkline/history had no other consumers; it was removed. Current rates and session totals remain visible in a compact text row.
- Menu-bar device and IMS rows now use concise text plus small state dots; the panel avoids large icon tiles and repeated sub-statuses.
- Activity status overview now summarizes device count, backend, combined VoWiFi, communication and (when enabled) fax rather than repeating detailed card contents.
- Sidebar rows now use explicit buttons and selected-state styling. This avoids relying on `NavigationSplitView` list-selection behavior for page changes.

## Validation

- BLE manager deterministic sequence suite: 24 groups passed, one iteration each.
- Targeted Go BLE manager regression suite: passed (stale BLE-presence probe, request deadline, selection locking, eSIM-operation and active-VoWiFi guards).
- Full macOS 27 arm64 Release build and DMG packaging: passed; image verification passed.
- Hardware UI smoke test: no ESTK/reader hardware was visible during this audit; real-device operation is not claimed.
- Signing: local ad-hoc signature; no notarization ticket was available/configured.

## Tradeoffs

- Removed only duplicate presentation controls and the unused menu-bar chart; kept controls that still have a distinct function.
- No telephony, proxy, NAS, power, or device configuration behavior was changed.
- No incoming-call test was attempted.
