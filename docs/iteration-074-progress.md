# Iteration 074：Celmux UIM home MCC/MNC 查询适配

日期：2026-10-05。全 Rust 后端持续目标下的 P2B/QMI 工作。所有修改均在 `/Users/proletarius/Desktop/Switch/src`；`/Users/proletarius/Downloads/celmux-src` 为只读参考。

## 完成内容

- 在 `proletarius-qmi-services` 新增窄版 `UimClient`，使用 QMI UIM service `0x0b` 的 Get File Attributes `0x0024`、Read Record `0x0021` 和 Read Transparent `0x0020`。
- 先读取 EF-OPL（`0x6fc6`），按 Go 一样的文件路径优先级、属性读取/首记录 fallback、最多32条记录和首个精确十进制PLMN规则查询 home MCC/MNC。
- OPL 无有效值时，回退读取 EF-IMSI（`0x6f07`）和 EF-AD（`0x6fad`）；保留 IMSI 长度字段、去除首个身份类型 nibble、MNC 长度 2/3 位识别及默认2位规则。
- Celmux 来源 `/Users/proletarius/Downloads/celmux-src/crates/qmi/services/src/uim.rs` SHA-256：`9c8f74ffc2e78ef0d57c5ef2dfe559502581d60ced815d140ee0db35f5b8e4a5`。该快照不含 Git metadata，SHA-256 用于固定本次审阅到的文件内容。使用 MIT crate 内的 UIM TLV 定义与文件属性布局；没有复制 Celmux SIM manager、PIN/写卡流程、其他 UIM 能力或平台设备 owner。
- 将 client error 类型抽成通用 `QmiServiceClientError`，旧 IMS/NAS 错误名保留为兼容 alias。

## 验证与限制

- `proletarius-qmi-services` 定向测试 **17项通过**：UIM 解码/EF-AD fallback 四项、Go-compatible IMS/NAS 八项、IMS/IMSA/NAS/UIM fake client lifecycle 五项。
- UIM fake transport 验证 service/client ID、service/message ID、session/file path、记录号和长度，以及首条无效 PLMN 跳过后采用下一条有效 home PLMN。
- 测试没有读取实体号卡；UIM facade 尚未接 `switchd-rs` 或唯一 QMI device owner，无法据此确认特定调制解调器/卡的真实 UIM 响应和运行稳定性。
- 没有生成 DMG；没有改变短信、呼叫或卡片状态，也没有触碰 9eSIM `6982`。

## 进度

本轮完善 QMI service facade，不是设备 owner 接管：Go 仍持有可达的 macOS QMI backend，Rust transport/IMS/NAS/UIM clients 尚未由 daemon 统一拥有。因此 P2B 仍 **22%**，全项目仍为 **20.0% 已完成 / 80.0% 待完成**（权重估算误差约 ±3 个百分点）。下一项应把选择后端、设备唯一 owner、service clients、重连/关闭和 Go 回退边界接成一条可测 Rust runtime，再评估生产迁移进度。
