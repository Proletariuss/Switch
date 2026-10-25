# Iteration 075：Rust 接管模块 AT 存储短信接收闭环

日期：2026-10-05。所有改动均位于 `/Users/proletarius/Desktop/Switch/src`；参考源码目录保持只读。

## 结果

Go modem manager 的 CMTI 与启动扫描在 Rust modem actor 已配置时，现在委托给 `switchd-rs` 私有 UDS 路由。Rust 在预期 SIM generation 下选择/查询 CPMS、读取 CMGL 并恢复之前的存储设置；复用 Rust 短信解码器和 multipart 冲突规则，将完整消息持久归档后，才对 ME 单条来源记录执行 generation、storage 和原始 PDU 三项复核及删除。SM、multipart、未知或变化中的来源记录均保留。未配置 Rust actor 的兼容宿主继续走已有 Go fallback。

这把生产流程推进到了 Rust 归档 owner 和短信来源清理 owner，但 Go 仍解析/分派 CMTI、触发启动扫描并保留旧环境 fallback；短信发送、IMS 与读卡器短信来源、其余 URC 副作用仍未迁移。

## Celmux 参考范围

本次 Rust actor 仍沿用已评估的 Celmux AT session 原则：单串口 owner、有限事务、URC 与命令应答隔离、storage 切换与读取不可被其他 AT 命令插队。没有复制 Celmux 的 AT session/port 文件；这些参考文件带 PolyForm Noncommercial 头，而且 Celmux 应用级 owner 不具备 Switch 的 SIM generation、durable ACK 与跨 reader/APDU 仲裁约束。Switch 的现有 Rust actor 是唯一硬件 owner。

本轮没有新增 Celmux vendor 文件。已有的 Celmux-derived Rust SMS/TPDU 代码按 Switch API、multipart 冲突策略与数据结构适配；Celmux 的 QMI/SIP/eUICC 扩展范围见 [复用评估](optimization/2026-10-04-celmux-reuse-review.md)。

## 验证

- Rust 伪终端端到端回归证明顺序为 `CPMS → CMGL → archive commit → CPMS → CMGR/PDU verification → CMGD`；模拟 modem 在收到 CMGD 前检查磁盘归档已经包含正确 sender/content。
- Rust 单元回归覆盖 CMTI 索引、启动扫描、SIM generation、multipart 只选择该通知所属组、未完成分段失败关闭与 durable archive。
- Go manager 与 Rust UDS executor 测试覆盖委托选择、参数/认证与兼容 fallback。
- `bash scripts/test-migration-gates.sh` 全量通过：workspace Rust 测试和格式检查、Go vet/兼容测试与协议 parity gates 均成功；`proletariusd` 本轮为 339 项通过，carrier replay 为 2,722 个向量通过。
- 完整输出保存在 [迁移门禁日志](../build/optimization/2026-10-05-celmux-informed-migration-gates.log)。

## 进度与限制

本接管将 P2B 从 22% 调至 25%，在固定的 47.5 计划人周基线上，将总体估算从 20.0% 调整到 **20.5%（9.76/47.5 人周；约 ±3 个百分点）**。分数只计这条生产路径的实现和离线验证，不代表全量 Go 退出或设备验收。

未连接实际读卡器/号卡、未读取或发送用户短信、未访问网络、未制作 DMG。9eSIM `6982`仍按项目目标排除。生产启动/旧数据兼容、daemon 崩溃恢复和硬件验收仍是后续门禁。
