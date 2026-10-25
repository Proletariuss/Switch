# Iteration 296：短信公开读取移除 Go 快照依赖

日期：2026-10-09

本轮将 Rust 短信公开列表的数据边界收紧为持久归档 + Rust USB 接收预览。Go 进程只在确认写入 Rust 持久归档后才更新其兼容缓存，因此 Go 快照是重复的易变副本，不再作为列表读取的前置条件。进程启动、Go 快照 RPC 未确认或子进程替换期间，已归档短信仍可正常列出。

清空操作仍会同时冻结公开列表与 Go 缓存分区，并仅把与本次归档记录 ID 精确匹配的 Go 副本交给兼容删除桥；这保留了删除竞态防护和缓存清理语义。Go 暂存缓存、IMS/设备短信生产者和旧业务运行时尚未迁出，因此这不是短信硬件链路的完整 Rust 接管。

验证：

- `cargo test --manifest-path rust/Cargo.toml -p proletariusd`：1192 passed，0 failed，3 ignored。
- `CARGO_NET_OFFLINE=true sh scripts/test-migration-gates.sh`：通过，包含 Go/Rust HTTP 方法与 HEAD 线协议、IMS/SDP/AMR 分帧兼容门禁。
- 首次门禁只报告新断言的 rustfmt 格式差异；格式化后完整门禁退出码为 0。

进度仍为 31.4%。本轮移除了一个公开读取的 Go RPC/快照依赖，但未迁移主要业务或硬件事实生产路径，不增加整体加权完成比例。没有连接读卡器、切卡、发送短信、生成 DMG 或执行实机 VoWiFi 验证。
