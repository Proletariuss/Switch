# Iteration 338：模块 SIM 身份与 USB AT 会话代次绑定

2026-10-10。源码与验证产物仅位于 `/Users/proletarius/Desktop/Switch`。本轮只用伪 USB AT 传输和离线测试；未连接或操作实体 SIM、未联系运营商、未拨号/发短信，也未生成 DMG。

## 变更

- Rust `sim-identity` 响应现在携带由 USB AT owner 产生的 SIM 身份代次。代次在新物理传输建立、owner 撤销和传输断开时更新；它独立于 USB I/O epoch，避免改变现有命令与短信部分结果的生命周期语义。
- Go VoWiFi SIM 会话只保存 Rust 返回的代次，并在每次模块 AKA 和 Profile 复核请求中原样传递。Rust 在当前串行事务内核对代次；旧代次在发出 QCCID/CIMI 或 CSIM/APDU 前失败关闭。
- Rust transport detach 会立即使旧 SIM 身份代次失效，但不改变 USB AT 通用 epoch。边界计数溢出时 owner 停止接受新操作。
- 迁移清单仍为 343 method/path（Rust 282 / Go 61）、127 Swift HTTP 调用点、354 审阅范围；本轮只扩展既有内部 DTO，不新增路由。

## 验证

- USB AT 定向测试 57 项通过，覆盖身份复核、Profile 变化、旧代次重连前拒绝、AKA 旧代次拒绝，以及短信快照在后续存储断线时保留已完成结果。
- Go VoWiFi SIM 身份适配定向测试通过；完整 `scripts/test-migration-gates.sh` 通过：Rust workspace **1,869 passed / 6 ignored**，daemon **1,331 passed / 3 ignored**；Go 全包/vet、Swift BLE manager/proof、carrier、USB/radio/HTTP/codec/SDP/IMS MESSAGE 差分通过。
- 契约清单 `--diff` 通过；扫描器 43 项测试通过；Rust fmt、Go fmt、`git diff --check` 通过。
- 完整门禁日志：[Iteration 338 migration gates](../build/optimization/iteration-338-migration-gates.log)。

## 进度与边界

P2B 保守增加 **+0.025 计划人周等值**，计入模块 SIM 生产会话的身份代次隔离；累计 **15.720 / 47.5 ≈ 33.1%**，估值误差约 ±3 个百分点。此切片没有迁移共享身份缓存的 prepare/read/commit、Go 兼容缓存发布、模块物理生命周期或 Go owner；VoWiFi/EAP/IMS/N待生产会话、语音媒体、实机验收和最终零 Go 构建/运行仍未完成。9eSIM `6982` 不在本轮范围。
