# Iteration 443：daemon-held Native VoWiFi session task registry

日期：2026-10-13

## 改动

- `NativeSessionServiceOwner` 现在按 opaque `SessionGeneration` 保存启动的 SMS/IMS session `Task`，不再把刚启动的生命周期句柄交还给调用方后脱离 daemon owner。
- owner 可按 generation 请求停止，并在下一次启动前回收已完成的 task；回收发现 worker panic 后拒绝新会话，保留失败状态。
- daemon shutdown 先停止 supervisor 和已持有 task；只有 session slot 已清理且 owner-held task 全部 join 后才继续停止 recovery worker。shutdown deadline 到期会保留原 owner 与任务，未确认清理不能被报告为成功。
- 任务注册表仍是私有、未接生产入口的生命周期基础设施；没有改变 `NativeOwnerReadiness`，没有公开 start/stop HTTP 路由，也没有把 native IMS/VoWiFi 标记为生产接管。
- 清理了源码目录中的陈旧 `rust/target` 编译缓存（目录报告约 69 GB）；正式缓存仍由 `Switch/build/rust-target` 管理。

## 验证与范围

- `cargo test --locked --manifest-path rust/Cargo.toml -p proletariusd`：**1451 passed / 3 ignored**。
- 定向测试验证 generation stop、worker panic 持久失败、监督槽位先移除时仍等待 owner-held task，以及恢复原 owner 后可继续 shutdown。
- `cargo check --locked --manifest-path rust/Cargo.toml -p proletariusd`、`cargo fmt --all --manifest-path rust/Cargo.toml -- --check`、迁移契约工具 44 项自测、`scripts/check-rust-migration-contracts.py --diff` 和目标文件 `git diff --check` 通过。迁移契约数及 HTTP/Swift census 未变。
- 总体进度仍为 **37.99%（18.045/47.5）**；本轮没有生产 session startup、SMS/call dispatcher、受保护 IMS 数据面、Go 退出或真实网络/设备验收，因此不增加生产接管进度。
- 未操作读卡器、实体 SIM、短信或运营商网络；9eSIM `6982` 仍排除。

## 后续

将 registry 与真实、已验证的 immutable session context 启动入口接合；同时把 IMS profile、protected network、SIP actor、SMS archive 和 call/media actor 作为一个有序 owner 组装。维持 admission gate 关闭，直到跨进程 reader lease、清理回执、取消/重启语义及硬件验收形成完整证据。
