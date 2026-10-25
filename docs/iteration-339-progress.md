# Iteration 339：Rust USB AT owner 权威缓存模块 SIM 身份

2026-10-10。源码与验证产物仅位于 `/Users/proletarius/Desktop/Switch`。本轮仅使用离线伪 USB AT 传输；没有操作实体 SIM、联系运营商、拨号/发短信，也没有生成 DMG。

## 变更

- Rust USB AT owner 现在在成功读取 SIM 身份后，以零化存储持有 ICCID/IMSI 快照，并将快照绑定到专用身份代次。每次身份快照刷新都会签发新代次，使旧 Go 会话即使遇到同一条仍打开的传输也会失效。
- 后续 `sim-profile-check` 与 `sim-aka` 内部请求只携带代次和必要挑战数据；Rust 在 owner 内解析身份快照，不再让 Go 把 ICCID/IMSI 回传作为卡片操作的依据。
- owner 撤销、断连或关闭时立即清除身份缓存。Rust 在卡命令前校验当前 owner、传输和身份代次；过期或未知代次 fail closed，不发出卡命令。
- 不增加公开路由；接口清单仍为 343 method/path（Rust 282 / Go 61）、127 个 Swift 调用点和 354 个审阅范围。

## 验证

- 完整 `scripts/test-migration-gates.sh` 通过：Rust workspace **1,870 passed / 6 ignored**，daemon **1,332 passed / 3 ignored**；Go 全包/vet、Swift BLE、carrier、USB/radio/HTTP/codec/SDP/IMS 差分均通过。
- USB AT owner 合成测试覆盖身份快照提交、刷新代次使旧快照失效、Profile 身份匹配/变化、断连后旧 Profile/AKA 请求零卡命令，以及挑战数据边界清理。
- 契约扫描器审阅更新与全量清单差异核验、扫描器 43 项测试、Rust/Go 格式检查和 `git diff --check` 通过。
- 完整日志：[Iteration 339 migration gates](../build/optimization/iteration-339-migration-gates.log)。

## 进度与边界

P2B 保守增加 **+0.025 计划人周等值**，累计 **15.745 / 47.5 = 33.147%（约 33.1%）**，估值误差约 ±3 个百分点。本轮只完成 SIM 身份缓存的 Rust 权威归属和代次绑定；Go 仍保留用于 EAP/IMS 配置和更高层 VoWiFi 编排的兼容数据，以及模块 reader 物理生命周期等依赖。共享 reader 生命周期、VoWiFi/EAP/IMS/N待生产会话、语音媒体、实机验收及最终零 Go 构建/运行依赖仍未完成。9eSIM `6982` 不在本轮范围。
