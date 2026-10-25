# Iteration 323：读卡器选择与逻辑代次迁入 Rust

2026-10-10。按用户要求，先交付冻结至 Iteration 322 的 [Switch 2.97 / 构建 297](release-2.97.md)，再继续本轮源码迁移。本轮开发改动不在该 DMG 内，也没有重新打包或替换已发布文件。

## 已切换的生产路径

Rust reader Gate 统一持有选中读卡器、逻辑 generation、连接时间、转换状态和回执。Go 的 Connect、Disconnect、RebuildSession 及已知 ICCID 改变后的身份刷新，必须取得 Rust 确认后才能发布新状态；原先直接修改 reader/generation 的位置统一改为投影回执。N待受控 Profile 切换继续保留其原有稳定代次语义。

- 新增内部认证的 `POST /api/internal/reader-lifecycle`，支持 prepare、commit、abort、snapshot，请求严格限制为 4 KiB。prepare 绑定 producer、token、sequence、原 reader/generation、目标与授权来源；commit/abort 必须保持同一不可变请求。
- 同一把 Gate 锁验证业务 capability、物理 worker 与转换状态。新的准入在转换期间失败关闭；借给转换的确切 capability 在终态确认前不能释放。新 operation permit 必须对应 Rust 当前已确认 reader/generation；已有 permit 可以完成其借权操作，并在代次变化后释放。近期已释放 token 不能立即复活，释放回执缓存仍有界。
- 已接受请求的重复 prepare/commit 返回一致回执，不再执行物理开卡、复位或其他 APDU。终态回执只在仍对应当前状态时可重放；后继选择、同名重连和旧 producer 不能被历史回执覆盖。未知结果保留原转换并隔离 Go 缓存，不把空投影当作断开成功。
- PC/SC commit 在 Rust Owner 元数据锁内取得当前 session ID/sequence，并保持锁直到 Gate 消费证明；rebuild/identity 还核对原绑定。丢卡清理必须提供实际关闭成功后的精确 pending handoff；关闭墓碑和未确认关闭不能作为证明。回执重试先于证明查询，因此已 ACK 的成功断开仍可确认。
- BLE 只读状态探测期间，逻辑断开仅借用该探测的精确 operation capability；不放宽普通业务对其他 worker 的权限。探测完成与转换按同一把 Go 投影锁协调，未知转换继续保留许可，终态确认且探测结束后才释放。
- 库存列表、eSIM 入口和聚合快照从 Rust 读取选择，不再恢复 Go 返回的旧 selected。读卡器信息在观察前后验证选择和代次，拒绝 pending、同名重连、重启后缓存身份与旧 Go generation。

所属 Go child 的真实退出由 supervisor 确认后，Rust 才退休 producer、清空逻辑选择并推进全局代次。尚未返回的 Rust 物理 worker 和 BLE 控制许可继续保留，不能因为 Go 退出而伪造物理结束。

## 审议发现与修复

并行审查覆盖 Go 状态投影/调用者、PC/SC 物理证明、Rust 状态机与消费端。发现并修复了 BLE 探测期间断开被误拒、旧代次申请新 operation permit、丢卡清理未知结果被后续空投影误确认等边界。

完整回归同时发现旧测试仍把 discovery.selected 当作权威。相关夹具改为显式局部选择输入，并保留真实 Unix 请求、租约申请/释放、原始请求体转发与租约后的权威复验；没有向进程全局 Gate 写入测试选择。另修复 BLE 测试 socket 名在同一时钟刻度下碰撞，加入进程内唯一序号。

## 验证证据

- 完整 `scripts/test-migration-gates.sh` 通过：Rust workspace **1,788 passed / 6 ignored**（daemon **1,259 / 3 ignored**）；Go vet、全包测试，以及 carrier、USB、radio、HTTP method/HEAD、codec、SDP 和 IMS MESSAGE 差分通过。
- Rust 生命周期、PC/SC 证明、读卡器信息与内部鉴权/请求体边界覆盖在完整门禁中。Go 生命周期、BLE 探测、热拔插和 PC/SC 专项 `-race` 通过；最新 Go 主包复核通过。
- 契约清单 **336 method/path（Rust 276 / Go 60）**、127 个 Swift HTTP 调用点、342 个审阅范围；扫描器自身 43 项测试通过。新增一条私有协议，不计总体完成率。
- `git diff --check` 与 Rustfmt 通过。测试输出、缓存和临时 Go 工具链 shim 均限定在 Switch/build；临时 shim 已清理。
- `cargo build --locked -p proletariusd` 的非测试 daemon 构建通过，日志为 `native-build.log`；只生成 Switch/build 内开发二进制，没有运行或安装 App。

最终日志位于 `Switch/build/optimization/2026-10-10-iteration-323-*`，以 `migration-gates.log` 和 `contract-check.log` 为最终完整结果。初轮门禁的 4 个旧选择夹具失败保留在 `migration-gates-first-failed.log`；独立 daemon 首轮 socket 碰撞仅保留真实尾部摘录 `daemon-after-fixtures-first-failed-excerpt.log`，不将该摘录称作完整日志。

## 保留边界与下一步

Go 仍承担物理生命周期适配、`authMu`、多命令事务协调、native lease/watchdog、卡片信息采集及真实 IMS/VoWiFi/N待会话。Rust 当前接管的是选择和代次权威，完整 reader owner 尚未退出 Go。

持续 IPC 故障导致转换无法确认时，当前实现保持隔离和占用；没有自动重放物理操作，也没有新增后台恢复队列。确切 producer 退出可解除该进程留下的逻辑状态；物理 worker 的实际退出仍独立核验。后续需把这一恢复边界纳入完整 Rust 生命周期，而非增加超时强制释放。

本轮未连接真实卡片、执行 APDU、下载 Profile、拨号或发短信；B0/B1 认证实现沿用已发布源码，未增加真卡验证结论。总体加权进度仍为 **31.4%**；readiness 中 reader_session 继续标记依赖 Go。下一步接管 reader 物理事务与 native lease/watchdog，完成全部消费者迁移后再移除 Go `authMu`，继续向零 Go 构建/运行目标推进。
