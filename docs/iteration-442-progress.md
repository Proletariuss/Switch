# Iteration 442：Native IMS session generation 控制

日期：2026-10-13

## 改动

- `SessionGeneration` 增加受校验的数值转换；supervisor 可按 daemon 签发的 generation 查询状态或请求停止。控制请求不需要提交 ICCID/IMSI，旧 generation 不能命中新会话。
- `NativeLifecycle` 增加按 generation 查询和停止的内部边界。当前未连接公开 API，也未把 prepared context/task 接入该边界。
- IMS actor 的 `Task` 增加完成状态查询，为后续 owner-held task 回收提供观察能力；停止仍是协作式 watch 信号，task owner 不能 abort 已启动的清理。
- 手动复核并更新迁移契约审阅指纹；HTTP/UDS method/path census 没有变化。

## 验证与范围

- `cargo test --locked --manifest-path rust/Cargo.toml -p proletariusd`：**1449 passed / 3 ignored**。
- generation 查询/停止隔离测试与 task 完成/回收观察测试分别通过。
- `cargo check --locked -p proletariusd`、`cargo fmt --all --check`、`scripts/check-rust-migration-contracts.py --diff` 通过。
- 总体进度维持 **37.99%（18.045/47.5）**：没有 daemon-held active task map、生产启动/停止路由、IMS profile 到受保护网络的生产接线，也没有改变任何 owner gate。本轮属于生命周期基础能力，不计生产接管量。
- 未连接实体读卡器、SIM 或运营商网络；未生成 DMG；9eSIM `6982` 继续排除。

## 后续

将 generation 控制扩展为 daemon-held session registry：安全持有每个启动 task，支持按 generation stop/status、完成任务回收及 shutdown drain；随后把已验证的 profile policy、IMS endpoint 和 SMS archive/subscription 来源接入 single-profile startup。只有路由、存储语义、lease-loss、SIP cleanup→IKE DELETE 顺序及重启/恢复测试闭环后才评估生产 gate。
