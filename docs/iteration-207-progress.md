# Iteration 207：Rust eSIM 写操作并发安全

日期：2026-10-09。继续推进 Switch 全 Rust 后端目标，仅在 Switch 源码目录内工作。未连接实体读卡器或 SIM，未下载、切换、改名或删除实体 Profile，未访问运营商，未生成 App/DMG。

## 变更

reader eSIM Profile 下载/切卡与普通 USB 模块 eSIM Profile 改名/删除共用一个进程级原子 RAII 操作闸门。闸门在写操作准入时先行原子预约，再核对耐久操作日志；已有 Rust eSIM 写任务、未核对的中断状态或日志故障都会拒绝新操作。reader 下载和切卡分别持有 permit 到既有 worker/终态收尾；模块改名、删除把 permit 所有权延续到 USB AT worker 返回，外层请求再持有到 radio lease 释放和兼容缓存刷新完成。因此取消 HTTP 等待方不会在实际 APDU worker 仍运行时提前放开闸门。

此修复补上模块 Profile 删除此前没有检查 Rust reader 操作状态的缺口，同时消除“先查询、后执行”允许两个 Rust 请求并发通过的竞态。Go 旧操作状态查询仍保留，但与 Rust 原子闸门不构成跨 Go/Rust 的同一把锁；非 USB fallback、Go 状态观察和缓存刷新仍是迁移依赖。

## 验证与阻塞

- 新增跨 reader download/switch 与 module mutation 的闸门回归测试通过。
- `module_esim` 定向测试 8 项通过；`reader_esim_download` 定向测试 73 项通过。
- `scripts/tests/test_rust_migration_contracts.py` 43 项通过；合同 census 与 fingerprint 审阅更新通过。
- `RUST_TEST_THREADS=1 bash scripts/test-migration-gates.sh` 完整通过，包含 Rust workspace、Go 全仓 test/vet、2,722 项载波回放及 USB/radio/HTTP method+HEAD/PCMA/SDP/AMR 差分。
- `cargo +1.98 fmt --manifest-path rust/Cargo.toml --all -- --check` 与 `git diff --check` 通过。
- 当前 census 为 334 个 method/path 契约（Rust 271、Go 63，含 49 个 HEAD alias）、127 个 Swift HTTP 调用点、306 个审阅范围。
- Rust-only 门禁仍被默认 Go fallback 与 177 条 Go owner/dependency 路由阻塞；构建退出审计仍有 7 类 Go 构建/运行/源码依赖，生产 Go 源文件 254 个。

## 进度

该改动加强 eSIM 操作安全，但没有让新的生产业务边界脱离 Go，因此不计迁移完成度。整体仍为 **14.845/47.5 计划人周等值（31.3%，误差约 ±3 个百分点）**。最新规划仍为约 **210–380 个自然日，中值约 290 天**；这是全职投入和既定实机/运营商等待风险下的规划区间，不是交付承诺。持续估算见[全 Rust 改造方案](optimization/2026-10-04-all-rust-backend-plan.md)与[执行看板](optimization/2026-10-04-all-rust-execution.md)。
