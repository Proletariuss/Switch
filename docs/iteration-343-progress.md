# Iteration 343：SM/ME 短信清空物理事务迁入 Rust

2026-10-10。修改和验证材料只在 `/Users/proletarius/Desktop/Switch`。使用假 USB AT 传输；未操作实体 SIM、发送短信、拨号或生成 DMG。

## 变更

- 公开短信清空请求现在由 Rust 冻结本地归档/USB 预览 ID，并把所选 SM、ME 分区作为固定操作直接交给 Rust USB AT owner；不再调用 Go 的 `/api/internal/sms/clear` 物理适配器。
- 清空结果只将同卡且来源全部确认的记录标记为实体删除；确认后才修剪 Go 重复缓存。物理清空失败时不执行后续本地归档清理，返回原有部分结果诊断且不自动重试。
- 仅清空本地记录继续执行精确 ID 删除，不触发 SIM/ME 命令；同时选择实体与本地清空时，先冻结本地 ID，实体清空确认后再按该快照删除归档和预览。
- 公开路由数量不变：343 method/path（Rust 282 / Go 61）、127 Swift 调用点、354 审阅范围。更新了清空物理 owner 的契约审阅记录。

## 验证

- owner 集成测试核实无 Go lease 的 Rust 入口仅执行请求的 SM 分区，并在相同 SIM 身份和已确认来源下返回删除 ID。
- 本地清空、存储 owner、公开 POST-only 方法测试通过。
- 完整迁移门禁通过：Rust workspace **1,873 passed / 6 ignored**，daemon **1,338 passed / 3 ignored**；Go 全包/vet、Swift BLE、carrier replay、USB/radio/HTTP/codec/SDP/MESSAGE 差分通过。
- 迁移契约扫描及 `git diff --check` 通过。
- 日志：[Iteration 343 migration gates](../build/optimization/iteration-343-migration-gates.log)。

## 进度与边界

P2B +0.025，累计 **6.325 / 8.0 = 79.1%**；整体 **15.845 / 47.5 = 33.4%**（约 ±3 个百分点）。监督模式下，公开单条删除、按发送方删除和 SM/ME 清空的物理操作现由 Rust 执行。Go 仍保留实体短信发送硬件适配、重复缓存修剪及 standalone fallback；真实设备验收、模块 reader 生命周期、IMS/VoWiFi 在线会话、N待网络、语音和零 Go 构建/运行依赖仍待完成。9eSIM `6982` 继续排除。
