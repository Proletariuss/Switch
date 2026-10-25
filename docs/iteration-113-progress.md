# Iteration 113 — 配置备份原始导出迁入 Rust

日期：2026-10-08。将认证后的 `GET /api/call-mode/backups/export?id=...` 从 Go 迁入 Rust `proletariusd`。保留 Go 的 ID trim/允许命名空间规则、原始 JSON 字节、JSON 下载类型、附件文件名和 `nosniff`；通过不跟随符号链接的文件句柄读取普通文件，拒绝空文件和超过64KiB的备份。支持 Last-Modified、If-Modified-Since、单段/后缀/多段 Range、If-Range 和 416 Content-Range。读取在既有有界 archive worker 池中执行。公开方法策略将 GET 的 HEAD 别名映射到同一个 handler，再仅去掉最终响应体。Go 导出handler与Swift导出HTTP测试已移除。

兼容范围：Swift 导出流程只依赖成功响应内的原始文件字节，仍保持一致。Go `ServeContent` 的基本条件请求、Range 分支有 Rust 回归；底层文件系统罕见竞态和系统错误文案未做逐字 Go/Rust 差分。Go 仍创建备份并负责导入、删除、配置修改和硬件恢复，故 call-mode 备份工作流仍未整体 Rust 化。

验证：Rust daemon **728通过、3忽略、0失败**；`call_mode_backups` 专项 **9通过**，覆盖原始字节与下载头、304、single/suffix/multipart Range、无效ID、符号链接与416。Go `cmd/proletarius-macos` 全测通过；迁移合同工具 **41项通过**，普通扫描通过，清单更新为**260**个method/path（Rust **177** / Go **83**，含**29**个HEAD别名）、**203**个审阅范围、**127**个Swift HTTP调用点。Rustfmt及`git diff --check`通过。未触发读卡器或实体卡操作。

Release `scripts/build.sh` 成功构建 `dist/Switch.app` **2.86 / build 286 / arm64**；`codesign --verify --deep --strict`通过，内嵌`switchd-rs`为arm64。不生成DMG。

P2A增加**0.025计划人周**，由3.550调整为**3.575/5.5（65.00%）**；整体由12.060调整为**12.085/47.5（25.4%，估算误差±3个百分点）**。该值只表示计划人周加权的已验证增量，不代表剩余备份写入或硬件调用已迁移。
