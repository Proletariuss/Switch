# Switch 3.43（构建 343）

2026-10-17。按用户要求，提供当前 **294/294 个后端接口由 Rust 接管**的阶段安装包。

[下载 DMG](/Users/StatTrak/Desktop/Switch_MAC/dist/Switch-3.43-macOS27.dmg)。Apple Silicon，macOS 27.0+。

- 从当前 Switch_MAC 源码构建；默认后端为 `switchd-rs`，包内 backend 仅含该 Rust 程序和必要的 libusb 动态库。SwiftUI 与系统桥保留。
- 本轮恢复四个历史 CLI 参数的解析及 VoWiFi 配置路径优先级、相对路径与按次重读；修复特殊路径显示导致 JSON 崩溃的边界。未改变运营商模板或 9eSIM 6982 排除范围。
- 完整 Rust workspace 3018 通过、0 失败、1 默认忽略；Swift BLE、无 Go 路径、接口契约与冻结协议门禁通过。294/294 后端接口归 Rust；此数字不代表业务实机验收率。
- Release 构建、严格签名、DMG 校验与只读挂载检查通过；直接从挂载包运行隔离后端，确认 Rust 原生状态及正常退出。

SHA-256：`b3e3792948d35554b14798d542b0b7c4987ac29cfb62d18f452b7860d2a4938c`。大小：48,716,480 字节。本机 ad-hoc 签名，未公证，未替换现有安装。ESTK/3HK 等实体卡、运营商 VoWiFi、通话/短信与长稳尚未在此构建验收；SIP 会话刷新、通话内媒体重协商和 CMR 编码器控制仍属后续软件工作。

[源码与验证记录](iteration-514-progress.md)。
