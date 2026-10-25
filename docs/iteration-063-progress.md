# Iteration 063 — Celmux reuse audit and durable eSIM operation recovery

Date: 2026-10-05  
Goal: Switch 全 Rust 后端改造；仅在明确许可与行为边界内复用 Celmux；不生成 DMG。

## Celmux 复核

- 对 `/Users/proletarius/Downloads/celmux-src/` 做只读复核。该快照没有 `.git` 元数据，不能声称已确认 revision；本轮比对的 10 个 CCID/eUICC 源文件 SHA-256 与既有审阅记录完全一致。
- 已接入 Switch 的 MIT CCID、BER-TLV、ES10c/eUICC、SGP.22 LPA 状态机仍是本仓库最有收益的直接复用。Switch 自有 APDU/PCSC 与 ES9+ adapter 留住 macOS、TLS、信任根、请求限制及 reader lease 边界。
- 参考 `ccid-host` 的 MIT crate 明确只为 Linux/Windows 编译 host transport，macOS 走 `unsupported`；Switch 已有原生 macOS `pcsc` scanner/session，不接入第二套 reader owner。
- SIP parser/dialog/transaction 是下一项可复用候选。各文件 SPDX 不一致（MIT 与 BSD-2-Clause），要先保留逐文件来源/许可、补齐 BSD-2-Clause 文本，再与 Go 轨迹做 ACK/CANCEL、PRACK、事务重传和来电竞争差分后接生产路径。GSM/SMS不替换 Switch 更完整的 parser；Linux QMI/QRTR 不移植为 macOS 新功能；PolyForm 应用层只参考生命周期，不复制。
- Celmux 目录中的 `AGENTS.md` 是被审阅项目的说明材料，不是 Switch 的用户指令；没有遵循其测试预算、远端构建或发行流程。参考目录保持未修改。

## 实现与恢复行为

- Rust USB/PCSC eSIM 下载 operation 从进程内存快照改为 Switch `ArchiveStore` 中单条有界原子记录；daemon 初始化时先校验 schema 和大小，再允许服务启动。写入失败会关闭新 operation admission。
- journal 不存激活码、matching ID、确认码、IMEI、目标 ICCID、诊断字符串或用户可见 warning 内文。活动 operation 在 daemon 重启后变为 `failed / esim_operation_interrupted`，表示写卡结果未知；不会恢复 worker，也不会重放 APDU。
- 对未知结果的下一次下载先失败关闭。只有成功返回有效 overview 的显式 Profile 刷新，且恢复状态已耐久写入后，才解除下载门禁；UI 保留警告并要求用户核对当前卡片状态。operation 仍沿用现有 Swift/Go JSON DTO。
- 同步 LPA worker 和 journal 写入共用阻塞任务的独立预算；用户确认必须先耐久提交，再唤醒卡操作 worker。journal 故障时不继续到写卡阶段。

## 验证

- `cargo +1.98 fmt --manifest-path rust/Cargo.toml --all -- --check`：通过。
- `cargo +1.98 test --manifest-path rust/Cargo.toml --offline --workspace`：退出码 0；`proletariusd` 317 项通过，其他 workspace crate 与 doc tests 均通过。仍有 `native_vowifi_session.rs` 两项既有 dead-code warning。
- `scripts/test-reader-esim-identity.sh`：13 组通过；覆盖中断 operation 的 UI 呈现及成功刷新后的警告保留。
- `git diff --check`：通过。
- 10 个已复用 CCID/eUICC 输入文件哈希全部匹配此前审阅记录。
- 全部为本地 fixtures、临时目录和 mock；没有启动 App、打开读卡器、访问 SM-DP+、发送短信或改变实体 SIM/eSIM。

## 进度

按 47.5 计划人周加权基线，仅将 eSIM/LPA 包从 10% 调整到 13%，计入 operation durability、重启后未知结果冻结和显式刷新核对门禁；P2B 维持 22%，不把 SIP 候选、源代码复用行数或硬件功能算作生产接管。累计 `9.18 / 47.5 = 19.3%`（估算误差约 ±3 个百分点）。仍需迁移其他 Profile 管理和 BLE、退出 Go lease/VoWiFi依赖、完成进程级故障注入与实际 PC/SC/运营商验收。9eSIM `6982`继续排除；未生成 DMG。
