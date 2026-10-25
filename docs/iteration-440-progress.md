# Iteration 440：通话音频入口迁移边界审计

日期：2026-10-13

## 结论

审计了 `POST /api/call/audio/start` 与 `POST /api/call/audio/stop`。Rust 已有 D4/UAC 语音路由 ADB start/stop/status、运行时代次、部署和完整性校验；但这两个公开入口仍包含必须一起迁移的协调语义：

- 启动前检查通话模式操作锁、USB composition、IMS/VoLTE 接口投影、ADB 可达性和模块运行时部署/代次状态。
- 启停都持有 Go `voiceUSBTransition` 独占锁，并通过 Rust USB AT owner 获取 `voice` lease；该 lease 会撤销旧 USB AT session。锁与读卡器/短信等 Go 调用方共享，避免 USB 重枚举与 AT/SMS 操作并发。
- Rust ADB helper 失败时，现有入口会查询路由状态并按已确认的最终状态决定是否返回成功；停止同样保留未确认错误。

因此仅把这两个 HTTP 路由登记为 Rust owner，或只调用现有 ADB route handler，都不能保持现有行为。该候选本轮不切换；先迁移共享 USB transition admission/调用方锁边界，再整体切换公开音频路由。此审计不增加后端迁移完成量。

## 当前进度与证据

- 总体迁移估算仍为 **37.99%（18.045/47.5）**；本轮没有新的业务 owner 接管。
- Go 退出审计仍有 6 类构建/运行/源码阻塞、266 个生产 Go 源文件。
- Switch 3.15/build 315 的 `/Users/proletarius/Desktop/Switch/dist/Switch-3.15-macOS27-iteration439.dmg` 已由本次 Release 构建产生；App 代码签名校验和 `hdiutil verify` 均通过。该 DMG 包含 Iteration 439 源码；本轮仅文档审计，不改变其内容。
- 未访问读卡器、SIM 或运营商网络；9eSIM `6982` 仍排除。

## 下一步

回到 Iteration 439 已确定的主线：把 Rust IMS transport 接入 keyed session actor，先在离线 fixture 中闭合单 Profile 注册、受保护 SIP 命令分派、取消与有序清理，再评估生产 owner gate。不得仅因 actor 单测通过就开启 VoWiFi 生产路径。
