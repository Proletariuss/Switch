# Iteration 336：模块 VoWiFi AKA 事务移交 Rust

2026-10-10。全部源码与验证产物位于 `/Users/proletarius/Desktop/Switch`。本轮仅进行离线合成测试，没有访问实体读卡器/SIM、没有连接运营商或拨打/发送短信，也没有生成 DMG。

## 变更

- Rust USB AT owner 新增私有、固定 schema 的 `POST /api/internal/usb-at/sim-aka`。请求只接受 USIM/ISIM 应用选择、预期 ICCID/IMSI 和固定长度 RAND/AUTN 十六进制；challenge 在请求对象销毁时清零，路由采用 4 KiB 请求上限。
- Rust 在单一串行 USB AT transaction 中重读身份、与现有 Profile 的 ICCID/IMSI 比对、按模块 EF_DIR 策略发现指定应用 AID，并发送/解析 AKA `AUTHENTICATE` APDU。仅跳过单条 EF_DIR 记录精确返回 `6986` 的情形；读卡身份变化时在认证 APDU 前失败关闭，物理命令不重放。
- Go 模块 EAP-AKA 适配器改为调用 Rust 并校验类型化结果，保留已有 EAP/IMS 会话消费逻辑和兼容错误分类。Go 原模块 AKA 的 EF_DIR/APDU 执行副本及对应过时测试删除。独立 Profile 复核、PC/SC/BLE AKA 路径、模块物理生命周期和 EAP/IMS 生命周期仍未迁移。
- 更新 API route/consumer census 与指纹审阅记录：342 method/path（Rust 281 / Go 61）、127 Swift HTTP 调用点、354 审阅范围。该统计不等同端到端 Go-free 完成率。

## 验证

- `proletarius-reader` 模块绑定 AKA 测试通过，确认精确 `6986` 处理以及身份变化时不发送 AUTHENTICATE。
- Rust daemon challenge 解码测试通过；Go `cmd/proletarius-macos` 全包测试通过。
- `scripts/test-migration-gates.sh` 完整离线门禁通过：Rust workspace **1,864 passed / 6 ignored**（daemon **1,326 passed / 3 ignored**）、Go vet/全包测试、Swift BLE manager/proof 测试、载波回放及 USB/radio/HTTP/codec/SDP/IMS MESSAGE 差分均通过。没有实机验收。
- migration census 一致性通过；契约扫描器 43 项测试通过；`cargo fmt`、`gofmt` 与 `git diff --check` 通过。
- 详细门禁日志：[Iteration 336 migration gates](../build/optimization/iteration-336-migration-gates.log)。

## 进度与剩余工作

按复核账本的生产职责权重记入 P2B **+0.050 计划人周等值**。累计 **15.670 / 47.5 ≈ 33.0%**，估算误差约 ±3 个百分点。进度衡量迁入并接入生产的后端职责，不是迭代数、路由数或测试通过率。

31.4% 是 Iteration 209 的旧累计值，之后没有持续按同一尺度重算；执行看板曾滞留在 15.420 / 32.5%，README 和账本则已更新至 Iteration 335 的 32.9%。本轮将三处统一到 15.670 / 33.0%，历史迭代记录仍保留其当时快照。

接下来迁移独立 Profile 复核与模块 reader lifecycle，再统一 PC/SC、BLE、USB AT 的身份代次发布和生命周期 owner；随后继续处理 VoWiFi/IKE/IMS/N待生产会话、完整语音媒体接管及零 Go 构建/运行验收。完整目标尚未完成，DMG 暂不生成。
