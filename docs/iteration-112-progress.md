# Iteration 112 — 配置备份列表查询迁入 Rust

日期：2026-10-08。把认证后的只读 `GET /api/call-mode/backups` 接入 Rust `proletariusd`，由 Rust 读取现有 `module-backups` 数据目录并投影 schema 0/2/3 列表字段。目录缺失返回空集合；拒绝不合法文件标识、符号链接、非普通文件、空文件和超过 64 KiB 的备份；格式错误文件保留在列表中并标为不可恢复。Go 不再注册该公开 GET handler。

范围限制：Go 仍创建备份并负责导入、删除、导出和硬件恢复。此改动只迁移只读列表端点，不表示 call-mode 备份工作流或硬件配置迁移完成。格式错误文件的诊断文本由 serde_json 生成，尚未证明与 Go `encoding/json` 的错误文本逐字一致；条目保留、无效状态与不可恢复投影已有测试覆盖。

验证：`cargo +1.98 test -p proletariusd` **725通过、3忽略、0失败**；Go `cmd/proletarius-macos` 全测通过；Rust迁移合同工具**41项通过**，普通源码合同扫描通过。更新后的清单包括**259**个method/path（Rust **175** / Go **84**，含**28**个HEAD别名）、**203**个审阅范围及**127**个Swift HTTP调用点。Rustfmt与`git diff --check`通过。Release `scripts/build.sh` 构建成功，产物为 `dist/Switch.app` **2.86 / build 286 / arm64**；`codesign --verify --deep --strict`通过，嵌入的 `switchd-rs` 确认是 arm64。未连接读卡器或实体卡，未进行备份恢复实机测试；本轮不生成DMG。

P2A增加**0.025计划人周**，由3.525升至**3.550 / 5.5（约64.55%）**；整体由12.035升至**12.060 / 47.5（约25.4%，估算误差±3个百分点）**。此百分比基于计划人周加权，并不代表备份硬件路径或全Rust目标已验收。
