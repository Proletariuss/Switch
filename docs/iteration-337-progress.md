# Iteration 337：模块 VoWiFi Profile 身份复核移交 Rust

2026-10-10。源码和验证结果仅在 `/Users/proletarius/Desktop/Switch`。本轮只用合成 AT 响应验证，没有连接或操作实体 SIM、没有联系运营商、没有拨号/发短信，也没有生成 DMG。

## 变更

- Rust USB AT owner 新增 `POST /api/internal/usb-at/sim-profile-check`。请求严格限定为 ICCID/IMSI，校验长度和数字格式；两条身份查询及比较在同一个串行 AT 事务内完成，输入身份缓冲在事务结束时清零。响应只返回匹配/已变化状态，不回传当前卡号，也不接受任意 AT 命令。
- QCCID 与 CIMI 加入有限身份命令白名单；保留 Go 原有 ICCID 去引号、去尾部 F 的规范化和 CIMI 数字解析语义。读取命令不会重放；身份不同继续返回原有“Profile 已切换，拒绝继续使用旧 VoWiFi 身份”的错误。
- VoWiFi 生产 Profile guard 改为调用 Rust 身份事务。Go 仍负责确保 USB AT owner 就绪、较高层 VoWiFi/EAP/IMS 生命周期和本地兼容互斥，但删除了 Go QCCID/CIMI 命令编排、比较器和 AT 回退路径。
- 更新 API 契约清单：343 method/path（Rust 282 / Go 61）、127 Swift HTTP 调用点、354 审阅范围。

## 验证

- Rust daemon 定向测试覆盖精确 QCCID/CIMI 解析、Profile 匹配、身份变化、无效输入在读卡前拒绝，以及两条命令在单一 owner transaction 内执行。
- Go `cmd/proletarius-macos` 全包测试和 Rust workspace 通过：**1,867 passed / 6 ignored**，daemon **1,332 passed / 3 ignored**。
- 完整 `scripts/test-migration-gates.sh` 通过，包括 Go vet/全包、Swift BLE manager/proof、2,722 carrier replay 与 USB/radio/HTTP/codec/SDP/IMS MESSAGE 差分。契约 census 一致性及扫描器 43 项测试通过；Rust fmt、Go fmt 与 `git diff --check` 通过。
- [Iteration 337 migration gates](../build/optimization/iteration-337-migration-gates.log)。

## 进度与边界

P2B 按实际生产职责迁移 **+0.025 计划人周等值**；累计 **15.695 / 47.5 ≈ 33.0%**（估值误差约 ±3 个百分点）。Go 独立 eSIM/Profile 工作流、USB/reader 物理生命周期、VoWiFi/IMS/N待网络会话、语音媒体、实机验收和最终零 Go 构建/运行仍未完成。9eSIM 6982 不在本轮范围。
