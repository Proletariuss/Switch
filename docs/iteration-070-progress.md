# Iteration 070：复用 Celmux QMI client/transaction core

## 完成内容

为避免重写 QMI 请求生命周期，从只读参考目录 `/Users/proletarius/Downloads/celmux-src/` 选取文件级 MIT 的 `crates/qmi/core/src/{lib.rs,fanout.rs,qmap.rs,transport/mod.rs}`，加入 `rust/crates/proletarius-qmi-core`。源快照没有 `.git` 元数据，故以文件 SHA-256 固定审阅版本：

| Celmux 源文件 | SHA-256 |
|---|---|
| `crates/qmi/core/src/lib.rs` | `ce716e4c3acb88b28b7033835bf374922fdffe20a9bfdf3188eba951dfd7fcd1` |
| `crates/qmi/core/src/fanout.rs` | `2237d947a1a0734f1e142c1679c1b908c151c6d1ceeb22ae18d448781e4c4d78` |
| `crates/qmi/core/src/qmap.rs` | `30fb1b8806f5bf1f1ace0eaa99aa318c8ccd1041c1e7d2d70e766cbec2eeb4f1` |
| `crates/qmi/core/src/transport/mod.rs` | `48cb8fcfd25533a659688c5a4ab19b8b413e23b95024f8fff7e0a870d62cfe97` |

复用 core 的并发事务匹配、transaction ID 管理、CTL client-ID 分配/释放、deadline 超时清理、迟到/不匹配响应隔离和有界 indication 队列。保留 MIT 文件头；将 wire 引用改为 Switch `proletarius-qmi-wire`。为适配 macOS 单一设备 owner，保留注入式 `Transport` trait，移除 Linux proxy 与 Windows USB 入口以及 Celmux logger 依赖。QMI manager、平台传输没有复制。

## 验证

- `cargo +1.98 fmt --all` 通过。
- `cargo +1.98 test --offline --locked -p proletarius-qmi-core`：3 项 fake transport 生命周期测试通过，覆盖 CTL client-ID 申请/释放、并发乱序响应匹配、超时之后迟到响应不会污染后续请求。
- `bash scripts/test-migration-gates.sh` 完整通过：Rust workspace（含 `switchd-rs` 336 项测试）、Go vet/全包兼容测试，以及 2,722 个 carrier replay 向量和 PCMA、SDP、AMR parity。
- `git diff --check` 与最终 `cargo +1.98 fmt --all -- --check` 在文档和代码改动收尾后复核。

所有请求都由内存 fake transport 回应；没有连接 USB/libusb、启动应用、操作读卡器或实体卡。`proletarius-qmi-core` 还没有 macOS transport，也未接入 `switchd-rs`，因此不计作 QMI 生产设备迁移。

## 进度与后续

总体进度保持 **20.0% 已完成 / 80.0% 待完成（9.51 / 47.5 计划人周，估算误差约 ±3 个百分点）**，P2B 保持 22%。这次复用减少了 QMI client 生命周期的重复实现，但没有转移生产设备 owner，不能按代码量增加完成度。下一步实现与 Go 一致的 macOS libusb interface 筛选、claim、CTL_SYNC 与有界读写，再以合成协议 fixture 差分；确认单一 Rust owner 后才接实际可达服务。9eSIM `6982` 仍排除；本轮未生成 DMG。
