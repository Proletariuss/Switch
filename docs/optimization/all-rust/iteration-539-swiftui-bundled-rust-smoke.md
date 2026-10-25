# Iteration 539：完整 SwiftUI 应用与包内 Rust 后端启动验收

日期：2026-10-18。

在上一轮 Release `Switch.app` 的副本上临时改用 `com.proletarius.switch.smoke` Bundle ID 并重新临时签名，使用独立 `SWITCH_DATA_DIR` 启动。启动参数将首次开机自启标记设为已配置、关闭自动更新检查；测试副本与已安装应用使用不同的偏好域，未安装或替换正式应用。仅导航活动、读卡器、N待和短信中心，没有开启 VoWiFi、切换/下载 Profile、发短信或拨号。

真实 SwiftUI 窗口显示“服务运行中 3.43”；其子进程是**该测试应用包内**的 `switchd-rs`，未启动 Go 后端。活动页显示读卡器 SIM World，读卡器页通过当前后端加载 ESTKme-RED 的 **10 个 Profile**，N待页显示 0/10 在线并列出 10 条线路，短信中心正常加载空对话。直接向后端发起未带应用令牌的 HTTP 请求返回 401，符合应用与后端的私有认证边界；UI 的运行中状态与卡片数据说明应用自己的带令牌请求可以工作。

通过应用的正常退出快捷键结束测试后，SwiftUI 主进程及其 Rust 子进程均消失，后端 `switch.sock` 已移除。BLE 桥的 `estk-ble.sock` 路径在进程退出后仍保留为陈旧 socket；当前 `ReaderBLEBridge.start` 会核对类型、探测是否有活跃监听者，仅对不可达的旧 socket 解链后重建。此项尚未进行退出后再次启动的实机回归，不把 BLE 清理判为完整验收。

脱敏进程与 UI 观察记录在 `build/reports/iteration-539-ui-smoke-sanitized.json`；确认两个进程均退出后，临时应用副本、运行目录和测试偏好域已删除。测试副本使用不同 Bundle ID 和临时签名，故这项证据覆盖当前源码对应的 SwiftUI 代码、包内 Rust 启动与页面读取，**不覆盖**正式 Bundle ID 的通知/权限、旧版数据升级回退、外部安装分发、SIM World IMS 注册/短信/通话或长稳。未生成 DMG。
