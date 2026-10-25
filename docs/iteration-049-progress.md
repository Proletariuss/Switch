> 历史记录：以下内容来自 Proletarius，包含当时的功能、路径、发布要求与验证结果，仅供追溯，不是 Switch 的当前指令或验收结论。Switch 已移除传真，当前构建与执行范围以根 README 和桌面工作区改造方案为准。

# 2.59 · 窗口型工作区视觉改造

日期：2026-09-30。版本 2.59，构建 259。

## 范围与实施

参考 https://lpa.nekoko.ee/zh 的蓝白配色、留白、圆角卡片与任务层级，在现有 SwiftUI / AppKit 上实现，不更换框架，不迁移后端。沿用 Proletarius 品牌，以原生矢量图标重新组织可改造页面。

1. 建立按页面显式启用的 `WorkspaceVisualStyle.nekoko`：自适应浅色/深色表面、按钮、表单、卡片与标题组件。
2. 重排活动、模块、读卡器、短信、通话、诊断、关于及设置页面，覆盖配套 eSIM 管理、下载、通知、重命名、短信详情/发送、通话记录和来电浮层。
3. 保留操作入口、业务调用、权限、禁用条件、确认与状态判断；仅调整呈现。动画遵守系统“减少动态效果”。
4. 改为 Dock 与菜单栏共存。主窗口注册与保持由独立协调器负责，关闭仅隐藏；Dock、菜单栏重新打开会恢复同一主窗口，包括已最小化的窗口。设置窗口与来电面板不参与主窗口注册。
5. 在交叉审阅中移除来电独立浮层中新引入的环境对象复制依赖，恢复原有文字选择与整块点击进入通话页行为，避免点击号码崩溃；修正菜单栏辅助功能计数文案。

## 保护边界

- 顶部隐私遮盖、外观按钮的原绘制、排列、透明点击区域及交互代码未修改。
- 传真页面使用 legacy 环境，并保留原侧栏样式；共享旧布局常量、配色、Logo 和卡片/按钮分支未调整。
- `FaxView.swift`、`FaxStore.swift`、`FaxRasterizer.swift`、`FaxModels.swift` 与两个共享 Logo 预览资源的 SHA-256 与本次实施前完全一致。
- 未调整后端、代理、NAS、SIM/APDU、IMS、SMS、媒体编解码或用户数据；没有做强制 UPX。

## 修改文件

应用壳与公共组件：

- `app/Resources/Info.plist`
- `app/Sources/ProletariusApp.swift`
- `app/Sources/MainWindowCoordinator.swift`（新增）
- `app/Sources/MenuBarViews.swift`
- `app/Sources/Views/ContentView.swift`
- `app/Sources/Views/GlassComponents.swift`

页面与配套弹窗：

- `ActivityView.swift`、`HomeView.swift`、`ReaderCardView.swift`
- `ReaderDownloadESIMView.swift`、`ReaderESIMNotificationsView.swift`、`ReaderRenameProfileView.swift`、`ModuleESIMView.swift`
- `SMSView.swift`、`SMSConversationViews.swift`
- `CallView.swift`、`CallKeypadViews.swift`、`CallRecentCallsView.swift`、`CallHistoryView.swift`
- `DiagnosticsView.swift`、`AboutView.swift`、`SettingsView.swift`
- `app/Sources/IncomingCallCard.swift`

除最后一项外，上述页面均位于 `app/Sources/Views/`。

检查与规范：

- `app/Tests/WindowLifecycle/main.swift`、`scripts/test-window-lifecycle.sh`（新增）
- `app/Tests/WorkspacePresentation/main.swift`、`scripts/test-workspace-presentation.sh`（新增）
- `DESIGN.md`、本记录以及版本/构建元数据与工程文件。

## 已验证

- 完整 Swift 源码类型检查通过。
- macOS 27 / Apple Silicon / 包含传真 Release 构建通过。
- 主窗口运行检查通过：注册、关闭隐藏、重开复用、设置窗口隔离、最小化恢复、重复注册及旧窗口替换。
- 浅色、深色组件渲染检查通过：传真 legacy 样式隔离，顶部保留图标在新旧视觉环境下逐像素相同。
- 读卡器 eSIM 身份 8 组、BLE 手动选择 13 例定向回归通过。
- 三路只读交叉审阅完成；本轮所发现阻塞问题已修正。
- 本轮已跟踪修改的文件 whitespace 检查通过。
- 只读挂载发行包核对通过：版本 2.59 / 构建 259、最低 macOS 27.0、arm64、`LSUIElement=false`；包内应用深层严格签名检查通过。

`docs/optimization/workspace-light.png` 与 `workspace-dark.png` 是生产共享组件的本机渲染示例，使用固定演示数据，不是实际读卡器联网状态截图。

## 未验证与留存事项

- 未重新执行真实 SIM 切卡、下载、VoWiFi 注册、来电/外呼、短信或传真收发；不能将构建或渲染成功当作真实业务验收。
- 主窗口协调器运行检查不等同于完整应用的 Dock / 菜单栏手动端到端验收；全部页面与各种设备状态仍需用户在实际窗口内验收。
- 现有 macOS 27 音频及拖入数据 API 废弃警告仍存在；本轮未为了消除警告改动业务层。
- 审阅发现既有 `CallRuntimeStore` 在模块 VoWiFi 激活时优先覆盖手选读卡器端点，留待独立业务修复，不在本次纯视觉升级中扩围。

## 发布

完整传真 macOS 27 / arm64 包：`Proletarius-2.59-macOS27.dmg`，保存至项目 `dist/` 与桌面 `test/`。发行脚本执行磁盘映像验证、签名检查及两份归档 SHA-256 一致性检查。默认本机 ad-hoc 签名，未使用 Developer ID 公证。

文件大小 42,903,712 字节，SHA-256：`4f81fbdcb58a6f108e7df94171c65ceb7d69d4d3f529986425ce4a505238251a`。
