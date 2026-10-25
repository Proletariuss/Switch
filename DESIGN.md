# Switch Interface Design

## Product character

Switch is a standard macOS window application for a connected 4G module and smart-card reader, with a persistent menu-bar entry. The current approved presentation takes its visual direction from https://lpa.nekoko.ee/zh: sky-blue actions, restrained rounded surfaces, generous separation, short contextual controls and a clear task hierarchy. Switch preserves the existing non-fax business capabilities and presentation. Its additional multi-profile SMS workspace uses a Möbius-loop sidebar icon.

## Protected presentation

- The global privacy and appearance buttons keep their existing drawing, transparent hit targets, toolbar order and behavior.
- Modern workspace styling is opt-in through `workspaceVisualStyle`; its default remains legacy for standalone surfaces. Do not change shared constants or logo resources to style an individual page.
- New artwork uses native vector symbols and adaptive colors. Existing shared logo resources are protected because they also serve the retained presentation.

## Native visual system

- Use semantic text/status colors and adaptive `WorkspaceColors` for modern page surfaces. Light mode uses near-white blue-gray backgrounds and white cards; dark mode uses blue-gray surfaces with brighter blue foregrounds. Application/system appearance must be honored consistently.
- Use the macOS system typeface and standard SwiftUI text roles. Page titles use `title2.bold`, panel titles use `headline`, body controls use `body` or `callout`, and supporting copy uses `caption`.
- Modern workspaces use a 24-point page inset, 18–20-point separation and 20-point panel padding, reduced on compact windows.
- Modern content panels use a 14-point continuous corner radius, adaptive surface/outline colors and a restrained shadow. Body buttons use an 8-point radius, with 28-point small and 32-point regular minimum heights. Blue distinguishes actions; green/orange/red remain status colors, not competing primary actions. Keep top toolbar icons free of background plates.
- Preserve native `NavigationSplitView`, segmented selection, toggle, text-editing, menu, sheet and accessibility semantics. Custom button/field presentation must keep bindings, permissions, disabled conditions and action results unchanged.
- Use semantic color on status icons; keep error and warning text in the primary foreground color for readable contrast. Secondary color is reserved for explanatory copy.

## Information architecture

- Modern sidebar groups are `概览`, `设备`, `通信` and `工具`, with transparent vector icons, quiet group labels and a soft blue selection state. All existing sections remain discoverable.
- Detail pages use the unified macOS title bar and toolbar. Global connection status and privacy masking stay available in the toolbar; page-specific actions use the same system-managed overflow behavior.
- Appearance, privacy, launch behavior, and menu-bar presentation live in the standard Settings scene. The existing `⌘1`–`⌘7` navigation commands, `⌘N` message composition and context-specific `⌘R` refresh shortcuts remain available.
- Major capabilities live as dedicated sidebar pages and remain isolated from unrelated device state.
- Avoid repeating the native title when it competes with the task. The reader page starts with a compact connection strip, then current-card/Profile management and Wi-Fi Calling tracks. Other pages may use a compact hero where it explains scope. Identity details, secondary configuration and histories remain discoverable.
- Runtime status, readiness, conflicts, and saved-state feedback stay near the mode selector and enable control.
- Hover and selection effects are brief; page/state changes use restrained opacity transitions. Honor Reduce Motion and avoid animation-driven network or device operations.

## Adaptive workspace density

- Layout follows the actual detail-area width and height, not display resolution. All detail pages receive `WorkspaceViewport`.
- Use horizontal room before adding page height. Module cards form two or three independent tracks, reader/card and Wi-Fi Calling stay parallel when they fit, activity uses balanced two/four-column arrangements, and about/update sections can sit side by side.
- On shorter windows, reduce decorative header size, padding and gaps, without dropping fields, controls, errors or changing disclosure state. Keep body information readable instead of globally scaling the page down.
- Keep SMS, call controls and sheet footers anchored in the available window. Scroll only overflowing histories, Profile lists, logs or long diagnostics within their own regions where possible. Measure short lists rather than reserving estimated rows of blank space.
- Whole-page scrolling is a safe fallback for genuinely overflowing content or narrower windows. Never disable scrolling or clip information merely to claim a one-screen layout.

## Reader state and presentation

- Preserve the reader page, Profile store, disclosure and task context across an in-progress Profile switch. Pending SIM identity replaces only identity-dependent facts, not the entire page. Never retain an old phone number, operator or IMS-ready badge as if confirmed.
- Mark a Profile as active only when its ICCID matches the currently read SIM. A cached enabled flag without current identity is pending confirmation. With no confirmed identity, starting Wi-Fi Calling is disabled but an in-flight or connected session can still be stopped.
- Keep scan, BLE, refresh, disconnect and reader enablement in the connection strip. Keep download, carrier notifications, enable/rename/delete, and all SIM/IMS details. Search filters names/providers only; masking must not leak matches to hidden identifiers.
- Reader tracks share the width from a 640-point content area, switching to one column below it. Large Profile lists scroll locally; the common two-profile list receives a small extra height allowance to avoid clipping its final number line. Do not claim all fields fit without scrolling in every state.

## Menu-bar presentation

- This menu-bar surface is separate from the protected main-window toolbar. Keep its label compact, without a count capsule; support the existing user-selected text/count options and privacy masking.
- The 320-point panel separates service, module, reader, SMS/call and traffic summaries. Module absence must not suppress an independently connected reader. Unknown states are not ready states.
- Opening a row navigates to the existing main window only. It must not implicitly connect a reader, start VoWiFi, read credentials.

## Window lifecycle

- `LSUIElement` is false and activation policy is regular, so Dock and Command-Tab coexist with `MenuBarExtra`.
- Register only the named main scene through `MainWindowRegistration` and `MainWindowCoordinator`; settings and incoming-call panels never become the main window.
- Closing hides and retains the main window. Dock reopening and menu-bar opening restore it, including deminiaturization; a repeated open action must not create another main scene.
- Silent launch suppresses initial window presentation only. Explicit Quit terminates the app and invokes the existing backend shutdown path.

## Call mode interaction contract

- 通话是侧边栏一级页面，也是一张持续存在的桌面工作台。首次准备、日常拨号、当前通话和恢复操作共用同一页面，不使用完成后消失的向导，也不把拨号、准备、记录和备份拆成同权重卡片。
- 主通话面保持为一个连续的有边框操作面。空闲时先显示号码输入，再以最近通话为主要日常区域、以紧凑的 3×4 矩形拨号盘为辅助输入，并且只保留一个醒目的拨打动作；最近记录可从行内直接回拨，完整记录另行展开。拨号、来电或通话开始后，当前通话状态在同一操作面内原位替换空闲工作台，右侧运行证据不随之移动。
- 宽窗口使用两条独立纵向轨道：可伸展的左轨依次放置主通话面与来电铃声，约 340 点的右轨依次放置完整运行检查器与模块配置备份。这样不同高度的次级内容不会被同一横向行强制配对。默认与窄窗口改为单列，不再把主通话面和检查器压缩成狭窄双栏；准备尚未完成时检查器在前，进入日常就绪状态后主通话面在前。只有最窄宽度把最近通话与拨号盘也改为纵向排列。
- 运行检查器只保留用于判断通话模式是否就绪的 `模块接口`、`运行时下载`、`模块部署` 三个稳定步骤。实际通话音频只在当前通话面显示，不作为准备阶段的等待项。完整检查器显示每步说明；紧凑检查器只压缩说明，不删除步骤名称、文字状态、当前准备动作或必要的下载与错误详情。等待用户确认的步骤使用静态 `待操作` 状态，只有已经发起的 USB 写入、重启、下载、部署或还原任务才显示不确定进度；状态变化只更新对应行，避免用户在模块重启与 USB 重新枚举期间失去上下文。
- 来电铃声和模块配置备份是常驻次级选项，不进入主通话面，也不使用 disclosure 或抽屉。宽屏中铃声跟随日常拨号轨道，备份跟随模块检查轨道；单列中二者位于主任务之后。铃声选择、试听、备份状态和刷新入口直接可见；备份仍默认只展示最近三项，避免与拨号争夺视觉层级。
- ADB 未授权或配置位已开但 daemon 不响应时，确认对话必须把 `QADBKEY` 持久授权与可还原的 USB 接口开关分开说明：密码只在本机计算且不保存，备份可以关闭接口但不能保证撤销授权。随后才说明会保留 VID/PID 与无关功能位、重启模块并短暂中断网络；只有用户明确确认后才执行。
- 模块接口行应连续显示 `等待授权`、`正在授权 ADB`、`正在开启接口`、`模块重启中`、`正在验证` 和最终结果。授权失败、配置漂移、重连超时与 ADB root 不可用必须使用不同文案并给出恢复动作。
- 配置备份列表只显示 IMEI 尾号、保存原因、时间、ADB/UAC 与 IMS/VoLTE 摘要；完整 IMEI 只存在于导出的 JSON 中。schema v2 备份明确标记为仅还原 USB，schema v3 同时还原语音配置。用户可以导入 Proletarius 导出的 JSON；导入时先校验格式、大小和结构化配置一致性，只保存到本机，不立即修改模块。每份本地备份都可以在明确确认后删除。
- `导出…` 是普通文件操作；`还原…` 仅在备份与当前模块身份匹配、通话空闲且没有其他模块操作时启用。还原确认必须说明会先生成保护备份、写入回读、重启和短暂断网。
- 运行时不随应用分发。模块接口准备完成后才显示并自动提示下载；原始源与国内转发渠道是并列选择，固定提交、文件大小和 SHA-256 校验规则完全相同。
- 下载进度、固定版本、本地路径和上游版本保持可见。文件只有完整通过校验后才原子保存，校验失败必须成为可复制、可重试的错误状态。
- `已就绪` 只用于真实 ADB root、UAC、IMS/VoLTE、本地运行时、模块部署和 D4/UAC 启停自检全部通过的状态。拨号与接听在此之前保持禁用，不能用配置位或乐观状态绕过准备过程。
- 通话状态为 `unknown` 时，主通话面必须原位显示锁定说明与重新同步动作；在刷新得到明确状态前，不允许新拨号、行内回拨或接听，也不把未知状态解释为空闲。运行检查器在此期间继续可见。
- 蜂窝通话进入 active 后才启动模块 D4/UAC 路由和 Mac 音频桥；挂断、启动失败和应用退出都应停止自有 helper 并回滚音频路由。
- 使用系统语义色、SF Symbols 与文字共同表达状态。错误必须同时说明问题和恢复动作，不能只靠红色或图标。

## Accessibility and copy

- Never communicate health or failure by color alone; pair status color with text.
- Decorative symbols are hidden from accessibility, while icon-only actions receive contextual labels.
- Keep routing names literal and consistent: `4G 直连`, `系统直连`, `系统侧 SOCKS`, `独立分流`, and `Clash 代管`.
- Explain ownership and traffic path in plain language. Avoid marketing language such as acceleration, protection, or global proxying.

## Multi-profile SMS workspace

- Use the Möbius-loop vector icon with the visible label `1 卡 N 待`. Keep the established reader and call pages intact.
- Separate per-Profile identity, connection generation, tunnel, IMS registration and SMS readiness. Do not present a physical enabled flag as proof of parallel sessions.
- SMS composition selects a confirmed Profile explicitly; incoming messages and receipts retain their Profile attribution. This workspace does not start voice or audio.
