# Iteration 205：普通 USB 模块 Profile 改名执行迁入 Rust

日期：2026-10-09。全 Rust 后端改造继续进行；本轮没有构建 App 或 DMG，也没有访问实体读卡器、SIM、运营商或处理 9eSIM `6982`。

## 实现

- `PATCH /api/esim/profile` 的普通 USB 模块路径现在由 Rust 处理：沿用现有请求大小、字段和名称长度边界，先查询活动 eSIM 操作，再申请共享 module-radio eSIM reservation。
- Rust 通过唯一 USB AT transaction owner 打开 eUICC 逻辑通道，调用既有 ES10c/APDU `set_nickname`，并在释放 USB 事务前关闭通道。失败继续返回兼容的 `rename_profile_failed`；成功保持原消息响应。
- 非 USB transport 请求仍转发到 Go；活动操作状态观察与成功后的概览缓存刷新仍依赖 Go。`DELETE /api/esim/profile` 保持 Go owner。未知/损坏的操作状态按忙碌处理，避免在状态不明时并发写卡。
- Go/Rust 冻结 HTTP 方法 oracle 纳入此前遗漏的 `PATCH` 与 `DELETE /api/esim/profile`，以验证两种方法的 405/Allow 行为。

## 验证

- 模块 eSIM fake-AT/APDU 测试：7 项通过，覆盖逻辑通道开/写/关、改名成功响应、AID 解码和活动操作失败关闭策略；没有发送真实 APDU。
- `RUST_TEST_THREADS=1 bash scripts/test-migration-gates.sh` 通过：Rust workspace daemon **1,109 passed / 3 ignored**，Go 全仓 `go vet`/`go test`、2,722 项载波回放、USB/radio、HTTP method+HEAD、PCMA/SDP/AMR parity 均通过。串行运行用于避免此前并行全量测试中的伪终端端口竞争。
- `python3 scripts/tests/test_rust_migration_contracts.py`：43 项通过；常规 route/consumer census 检查通过。
- 最新契约清单：334 条 method/path（Rust owner 270、Go owner 64，含 49 个 HEAD alias）、127 个 Swift 调用点、299 个审阅范围。Rust-only 检查仍失败：默认 Go fallback 存在，177 条路由仍有 Go owner 或依赖；这条改名路由仍记录操作状态查询、非 USB fallback 和缓存刷新依赖。
- `cargo +1.98 fmt --all -- --check` 与 `git diff --check` 通过。

## 进度与未完成项

按既有口径保守计入 eSIM/LPA **+0.025 计划人周等值**：由 **1.150/5.5** 调整至 **1.175/5.5（21.36%）**；整体从 **14.795** 调整至 **14.820/47.5（31.2%）**。这只计普通 USB 改名 APDU 执行和公开路由接线，不代表 eSIM 改名全路径无 Go，也不代表实体卡验收。

显式刷新、Go operation observer/cache、非 USB reader 兼容路径、Profile 删除/切换/下载其余流程、完整 LPA 生命周期、实体卡回放、Go-free 构建运行、IMS/N待生命周期和最终长期稳定性仍未完成。下一步沿着共享 USB owner 收敛 eSIM operation/status/finalizer 边界，并继续推进生产 IMS keyed lifecycle actor；优先移除真实 Go runtime 依赖，不能把路由 owner 标签当作全 Rust 完成。
