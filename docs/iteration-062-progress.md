# Iteration 062 — USB/PCSC eSIM download route to Rust

Date: 2026-10-05  
Goal: Switch 全 Rust 后端改造；参考 Celmux 的 operation/设备所有权边界；不生成 DMG。

## 本轮实现

- 将当前已选 USB/PCSC reader 的 eSIM 下载入口接到 Rust daemon，保留公开 API、Swift确认交互及operation DTO；BLE 继续通过 Go 兼容路径。
- Rust 开始任务前查询Go operation状态、关闭该读卡器 VoWiFi并取得 generation-bound reader lease；lease心跳与per-reader操作锁覆盖同步APDU/LPA工作流，Go在该lease期间拒绝旧eSIM operation。
- Rust worker执行AID/EID/NVRAM预检、大小估算与用户确认、现有LPA/ES9+传输、安装通知处理及结束后的reader刷新。工作线程预算为1，确认等待上限5分钟，每条APDU之前检查取消/lease丢失。
- 进度仅计离线证据证明已接入的USB路径；Rust operation snapshot仍是进程内存状态，崩溃/重启恢复账本、未知写卡结果冻结、Go lease/VoWiFi依赖退出、BLE迁移和硬件验收都没有计入完成。
- Celmux应用下载层的PolyForm Noncommercial源码仅参考operation生命周期；未复制该应用层代码。运行使用Switch已有MIT LPA/eUICC crates与Switch自有服务边界。9eSIM `6982`专项继续排除。

## 验证与边界

- `cargo +1.98 test --manifest-path rust/Cargo.toml --offline -p proletarius-lpa -p proletariusd`：LPA **24项**、daemon **314项**通过，0失败。
- `cargo +1.98 check --manifest-path rust/Cargo.toml --offline -p proletariusd`、`cargo +1.98 fmt --manifest-path rust/Cargo.toml --all -- --check`、`git diff --check`通过。
- `cd backend && go test ./cmd/proletarius-macos`通过；链接器只输出重复 `-lusb-1.0` 的warning。
- Rust daemon仍显示 `native_vowifi_session.rs` 两项既有dead-code warning。
- 所有APDU/ES9测试都是fake/local fixtures；未运行App、连接读卡器或实体卡、访问SM-DP+、安装Profile或执行9eSIM专项；未生成DMG。

按方案第7节中值47.5人周计算：P2B由20%调至22%（+0.16人周），eSIM/LPA由0%调至10%（+0.55人周），其余工作包保持不变，累计 **9.015 / 47.5 = 19.0%已完成 / 81.0%待完成**，误差约±3个百分点。这个百分比是工作包估算，不代表实机可用或已发布。

详情见[全 Rust 实施记录](optimization/2026-10-04-all-rust-execution.md)与[Celmux复用评估](optimization/2026-10-04-celmux-reuse-review.md)。
