# Iteration 114 — 备份导入持久化迁入 Rust，并居中呼入操作按钮

日期：2026-10-08。将认证后的 `POST /api/call-mode/backups/import` 从 Go handler 转交 Rust。Rust先读取不超过64 KiB的请求体，校验JSON、备份schema、保存时间、USB VID/PID/功能位、还原命令以及当前/旧schema规则。旧版 `schema_version: null` 继续按Go的零值语义解析。通过校验后，Rust在共享 `module-backups` 目录内以0700目录/0600文件权限创建暂存文件，写入并同步原始JSON，再通过硬链接创建时间戳+IMEI末四位名称，重名时沿用Go的 `-2` 起始序号；写完重读验证后返回原有 `201 {backup: ...}` 摘要结构。拒绝体积超限、空数据和篡改还原命令；读取超限返回400，校验/写入错误为400，写后验证错误为500。

Go删除导入handler、导入文件写入实现及其专属测试；`decodeCallModeUSBBackup`、验证、摘要、创建、删除和硬件还原仍由Go其他路径使用，因此这只是备份导入的业务边界迁移，不是整个备份工作流迁移。`serde_json`畸形输入诊断字符串未承诺与Go `encoding/json`逐字一致。

同时，呼入时接听与挂断按钮原先放在自适应 `LazyVGrid` 中，导致网格按左边缘排布。现在将操作作为一组横排控件，并让该组在通话卡片可用宽度内居中；按钮行为、就绪门槛和拒接语义保持不变。

验证：Rust daemon **731通过、3忽略、0失败**；Go `cmd/proletarius-macos` 全测通过；通话正确性回归通过；完整 Release `scripts/build.sh` 构建出 `dist/Switch.app` **2.86 / build 286 / arm64**，严格签名验证通过。Rustfmt、`git diff --check`、契约扫描及41项工具自测均通过。未使用读卡器/号卡，不发送短信，不生成DMG。

P2A增加**+0.025计划人周**，由3.575调整为**3.600/5.5（65.45%）**；总进度由12.085调整为**12.110/47.5≈25.5%（误差约±3个百分点）**。备份创建、删除、硬件恢复以及完整Go-free验收仍未完成。UI布局修正不计入Rust迁移百分比。
