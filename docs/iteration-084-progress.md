# Iteration 084：参考 Celmux 的 QMI owner 有界恢复

日期：2026-10-05。源码和产物均限定在 `/Users/proletarius/Desktop/Switch`；`/Users/proletarius/Downloads/celmux-src/` 只读参考。

## 改动

在 Switch 现有 Rust QMI owner 中落地一套窄范围恢复策略。该 owner 仍只提供类型化 IMS/NAS/UIM 控制操作，不暴露原始 QMI frame：

- USB transport 断开、关闭状态或服务句柄丢失时立即丢弃旧 owner 并重新打开选定 control path。
- 单个 operation scope 的普通 QMI timeout 不立即重连；同一 scope 在 60 秒内出现第 3 次 timeout 才恢复。其他协议/业务错误不触发 owner 重建。
- `probe` 和 `operator` 是只读请求，owner 恢复后最多重试一次；`set-ims` 是状态写入，错误结果可能不确定时只恢复后续请求并返回原错误，绝不自动重放写入。
- 首次打开或后续重开失败时保留原 control path供后续请求恢复；退避从 500 ms 指数增长到 30 秒上限。显式 close 清除路径并阻止隐式重开。
- 保持 Go 兼容行为：NAS operator 名称读取失败继续回空值；QMI 仍须显式选择，Go 模式编排尚未迁移。

Celmux `crates/qmi/manager/src/lib.rs` 与 `services.rs` 是 MIT 参考快照，SHA-256 和边界见 [`THIRD_PARTY_NOTICES.md`](../THIRD_PARTY_NOTICES.md)。本轮没有复制 manager 源代码，也未新增第三方依赖；实现为 Switch 自有 fakeable session/factory 与单-owner状态机。

## 验证

- 新增 14 项 fake-session 单测：transport loss / stale handle 恢复、连续 timeout 阈值、永久错误不恢复、mutating RPC 不重放、初次打开/重开失败后保留路径并可恢复、退避增长与 30 秒封顶、显式 close、timeout scope reset。
- `cargo +1.98 fmt --manifest-path rust/Cargo.toml --all -- --check` 通过。
- `cargo +1.98 test --manifest-path rust/Cargo.toml --offline -p proletariusd`：**361 passed, 0 failed**。保留既有 `native_vowifi_session.rs` 两条 dead-code warning。
- 契约扫描维持 196 个 method/path（Rust 93、Go 103）与 127 个 Swift HTTP 调用点；QMI endpoint 的 method/path、owner 与指纹不变，仅文件定位行更新。
- `git diff --check` 通过。

没有枚举 QMI USB 设备、连接实体模块、写 SIM/eSIM、发送短信或生成 DMG。该轮没有验证运营商 QMI 行为或真实 USB 断连恢复。

## 进度

只把离线验证过的 owner 恢复逻辑计入 P2B，从 33.7% 调整到 33.9%，增量为 0.016 计划人周。固定 47.5 人周基线下，总量为 **10.467 / 47.5 = 22.0% 已完成**，约 78.0% 待完成，估算误差约 ±3 个百分点。生产 QMI 仍受 Go 模式编排与硬件验收约束；整体全 Rust 目标尚远未完成。
