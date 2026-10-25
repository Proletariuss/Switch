# Iteration 344：USB AT 短信发送物理提交迁入 Rust

2026-10-10。所有修改和验证材料均位于 `/Users/proletarius/Desktop/Switch`。只使用假 USB AT 传输；未操作实体 SIM、未发送真实短信、未拨号或生成 DMG。

## 变更

- Rust USB AT owner 新增固定 `submit-sms` 操作：沿用现有 producer/session/SIM lease，在同一物理会话先复核预期 ICCID，再执行 PDU 模式和分段 `CMGS`；Rust 负责 PDU 编码与回执计数。
- 任一会话、卡身份、SIM lease 或 VoWiFi 射频 inhibit 复核失败时不发出短信写命令；关停时返回明确的 `usb_at_stopping`。提交结果不完整时标记为未知，禁止自动重放。
- Go 直连 USB AT 发送分支不再自行编码 PDU 或执行 CMGF/CMGS，改为调用 Rust owner。Go 仍保留高层通道选择、现有互斥/账本和 serial/IMS 分支，因此这轮只计 USB 硬件执行职责。

## 验证

- Rust USB AT 定向测试：**64/64 通过**，覆盖成功提交、卡身份变化、射频 inhibit、关停前零硬件 I/O。
- Go USB AT/设备短信定向测试通过。
- 完整 `scripts/test-migration-gates.sh` 通过：Rust workspace **1,879 passed / 6 ignored**；Go 全包/vet、Swift BLE 物理完成测试、carrier replay、USB/radio/HTTP/codec/SDP/MESSAGE 差分通过。日志：[Iteration 344 migration gates](../build/optimization/iteration-344-migration-gates.log)。
- 迁移契约扫描通过：**344 method/path（Rust 283 / Go 61）**、127 Swift HTTP 调用点、354 审阅范围；`git diff --check` 通过。

## 进度与边界

P2B +0.025，累计 **6.350 / 8.0 = 79.4%**；整体累计 **15.870 / 47.5 = 33.4%**（精确比值 33.41%，工程估值约 ±3 个百分点）。进度上调来自真实生产 USB 发送执行从 Go 迁至 Rust，不是测试数量增加。真实卡测试、SMS 实机验收、serial/IMS 发送迁移、高层通道选择与账本完全接管、模块 reader 生命周期、VoWiFi/IMS 在线会话、N待网络、语音和零 Go 构建/运行依赖仍未完成。9eSIM `6982` 继续排除。
