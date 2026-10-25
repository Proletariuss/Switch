# Iteration 206：普通 USB 模块 eSIM Profile 删除执行迁入 Rust

日期：2026-10-09。全 Rust 后端改造继续进行；本轮没有构建 App 或 DMG，也没有访问实体读卡器、SIM、运营商或处理 9eSIM `6982`。

## 实现

- 普通 USB 模块的公开 `DELETE /api/esim/profile` 路径现在由 Rust 执行。请求体采用有界解码并拒绝未知字段，ICCID/AID 使用现有格式校验。
- 删除前仍通过兼容 Go observer 确认没有进行中或状态未知的 eSIM 操作；随后获取 module-radio reservation，并在唯一 USB AT transaction 内打开 ISD-R 逻辑通道。
- eUICC 删除行为抽取为共享 `delete_profile_on_euicc`，复用 ES10c 删除、SGP.22 删除通知、空间变化核对及原有警告语义。逻辑通道在释放 USB transaction 前关闭。
- 非 USB transport 保留 Go 兼容回退；操作状态观察和成功后的旧概览缓存刷新仍依赖 Go。实体卡/Profile 不会在此实现过程中被操作。
- `DELETE /api/esim/profile` 的 Rust ownership 和冻结 Go/Rust HTTP 方法 oracle 已更新；契约 review 与源码 census 均同步。

## 验证

- `module_esim::tests`：8 项通过；新增 fake-AT/APDU 回放确认删除只发一次、未确认通知保留警告、逻辑通道关闭顺序正确。
- `reader_esim_download::`：72 项通过；共享删除核心和原有 reader 删除流程回归通过。
- `ownership::tests::module_profile_rename_and_delete_are_rust_owned`：1 项通过。
- `RUST_TEST_THREADS=1 bash scripts/test-migration-gates.sh` 完整门禁通过：Rust workspace daemon **1,110 passed / 3 ignored**；Go 全仓 `go vet`/`go test`、2,722项载波回放、USB/radio/HTTP method+HEAD/PCMA/SDP/AMR parity 均通过。
- `python3 scripts/tests/test_rust_migration_contracts.py`：43 项通过；route/consumer census一致；Rust格式检查及`git diff --check`通过。
- 最新契约清单：334 条（Rust owner 271、Go owner 63，含49个HEAD alias）、127个Swift HTTP调用点、303个review scopes。Rust-only退出检查仍失败：默认 Go fallback 存在，177条路由仍有 Go owner/dependency；独立 backend exit 审计列出7类Go构建/运行/源码依赖。

## 进度与剩余工作

保守计入 eSIM/LPA **+0.025计划人周等值**，从 **1.175/5.5** 调整至 **1.200/5.5（21.82%）**；总体从 **14.820/47.5** 调整至 **14.845/47.5（31.3%）**，估算误差约±3个百分点。P2A 74.7%、P2B 67.5%、P3 20.5%、P4 16.7%维持不变。

活动操作查询、非 USB 兼容、成功缓存刷新、Profile 显式刷新/启停/下载与完整 LPA lifecycle仍有Go依赖；Go runtime移除、IMS keyed lifecycle actor、N待多Profile实机与长稳、设备/运营商兼容，以及零Go构建与运行验收仍未完成。下一工作包应继续收敛 eSIM operation/status/finalizer owner，并打通 IMS keyed actor 的生产生命周期。此次不是端到端 Go-free，也不是实机验证。
