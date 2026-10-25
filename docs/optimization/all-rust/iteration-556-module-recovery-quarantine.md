# Iteration 556：旧锁仅隔离模块业务与无硬件同目录验收

2026-10-18。本轮延续 [Iteration 555](iteration-555-inplace-bundle-upgrade-blocker.md)：同一合成目录中的旧 3.42 空锁使新 Rust 的 call-mode finalizer 无限等待未知 ADB 恢复。目标是保留历史恢复证据，让本进程在没有接受任何模块物理工作的情况下正常服务读卡器并完成退出；不是宣称历史模块已恢复。

## 当前证据与范围

- 555 原始失败报告与样本保持不变：`build/reports/iteration-555-inplace-bundle-roundtrip.json`，数据读写往返成功、整体验收失败；旧 3.42 自身退出 1 是独立残余。
- 默认真实数据根只做过授权的锁/marker **元数据**检查，没有读取业务内容或修改文件：`build/reports/iteration-556-default-root-lock-metadata.json`。发现两个旧空锁，无 pristine、backup、voice-runtime；因此撤回全局启动 fail-fast，避免整个读卡器产品无法启动。
- 实现使用启动时一次性、不可重新启用的 Gate。preflight 只读：锁/marker 仅读元数据；若存在候选下载缓存，复用 no-follow verifier 校验当前 manifest 的有界文件字节（总计约 1.08 MiB）。不建立、删除、截断或重新分类任何恢复证据。监听、PCSC/BLE、reader lifecycle、N 待、ordinary reader IMS 分支保留。
- 未发起硬件验证、未覆盖 `/Applications/Switch.app`，没有使用真实业务数据、代理或生成 DMG。shipping daemon 的后续验收必须继续使用 546 mandatory hardware/network sandbox、复制 bundle 和合成根。

## 实现与公开契约

`module_recovery::Gate` 对未知旧 lock、模块 backup、未知 voice-runtime、无效 marker 或不可读证据投影 `legacy_recovery_required`，并始终 `historical_recovery_confirmed=false`。正常 clear Gate 的后续本进程工作仍沿用原有生命周期栅栏，不根据运行期间文件变化切换为隔离。

同一个 Gate 传入 serial actor、USB factory owner、radio admission/physical fence。隔离启动不构造 module_call/call_mode，不启动 module radio、USB SMS 接收、module startup、QMI；reader 服务与调度仍启动。隔离动作在物理 admission/factory 和内部 RPC 前拒绝，不能只依赖 HTTP 拦截。

公开 API 不增加路由：`/api/health` 仍 200、ok=true，模块 esim_available=false 与 discovery_error=legacy_recovery_required；`/api/status` 返回可解码的模块 unavailable DTO；snapshot 可保留 reader 分支。`/api/v2/runtime` 仅隔离时增加 typed `module_recovery`。模块物理业务/恢复证据修改为 409 typed code；未授权仍 401，既有方法/HEAD 行为保留。call-mode 状态为只读投影，voice-enabled 仅读已有缓存，不查询 AT、不建立 JSON lock。SMS devices 的模块分支不可用、reader 保留；通用 SMS 仅在模块 capture/claim 前拒绝，既有 accepted/unknown 结果保留。模块备份 list/export 仍可只读，import/delete/create 被挡。

退出只省略从未构造的模块通话 finalizer。若 owner 意外构造仍执行其原有保守 finalizer；radio、USB、serial owner 仍分别排空确认，reader/QMI/shared coordinator 保留。隔离 idle owner 可退出 0，表示**本进程**排空成功，绝不表示历史恢复成功。已有或未知物理动作不得用隔离状态伪造清理回执。

**pristine 证明边界：**有效 `.call-mode-pristine-v1` 只能解释由受控新版本启动、只读操作留下的空锁。未经包装的旧版回退可能保留 marker 后又改变模块状态，单凭该 marker 不能证明它安全。本轮不声称完整回退验收；555 的旧 3.42 → 新版本无 marker 用例应先证明隔离、证据保留和正常退出。可靠的旧版回退标识/后续显式恢复流程仍另行设计，绝不从空文件或缺少 backup 推断设备恢复。

## 精确改动文件（相对 src）

生产文件：

- `rust/crates/proletariusd/src/module_recovery.rs`（新增纯 preflight、typed status、模块路由分类）
- `rust/crates/proletariusd/src/availability.rs`
- `rust/crates/proletariusd/src/main.rs`
- `rust/crates/proletariusd/src/modem_actor.rs`
- `rust/crates/proletariusd/src/modem_actor/sms_receive.rs`（共享 ActorClient 保存 Gate，Weak 重建不能丢失）
- `rust/crates/proletariusd/src/usb_at.rs`
- `rust/crates/proletariusd/src/module_radio.rs`
- `rust/crates/proletariusd/src/call_mode_lifecycle.rs`
- `rust/crates/proletariusd/src/call_mode_adb.rs`（direct probe/runtime/voice worker 在触达 ADB 前同 Gate）
- `rust/crates/proletariusd/src/call_mode_status.rs`（传递同 Gate）
- `rust/crates/proletariusd/src/call_audio.rs`（传递同 Gate）
- `rust/crates/proletariusd/src/device_sms_send.rs`
- `rust/crates/proletariusd/src/health_projection.rs`
- `rust/crates/proletariusd/src/device_status_projection.rs`
- `rust/crates/proletariusd/src/sms_device_service.rs`
- `rust/crates/proletariusd/src/voice_enabled.rs`

测试/脚本/本文：

- `rust/crates/proletariusd/src/module_quarantine_tests.rs`（新增真实 HTTP dispatcher + anonymous socket pair）
- `rust/crates/proletariusd/src/modem_actor/quarantine_tests.rs`（新增 factory trap）
- `rust/crates/proletariusd/src/modem_actor/sms_receive_tests.rs`（三个 client fixture 加 default Gate）
- `rust/crates/proletariusd/src/usb_at/quarantine_tests.rs`（新增 factory trap、未知本次工作负例）
- `rust/crates/proletariusd/src/module_radio/quarantine_tests.rs`（新增 cleanup admission 拒绝/idle receipt）
- `rust/crates/proletariusd/src/device_sms_send/tests.rs`（模块 claim 前拒绝、不重写 accepted）
- `rust/crates/proletariusd/src/device_sms_send/native_reader/tests.rs`（模块隔离期间 reader IMS 正向发送）
- `scripts/check-p6-inplace-bundle-roundtrip.py`（保留原模式，增加 opt-in --expect-module-quarantine、typed 投影/模块拒绝/证据元数据/15 秒退出上限断言）
- `docs/optimization/all-rust/iteration-556-module-recovery-quarantine.md`

没有编辑 vowifi/auth/transport 协议源码、README、冻结 inventory。独立 NAT/IKE 代理可能同时编辑其他源码，不属于此清单。

## 已执行验证与待办

使用 Rust 1.98.0、offline/locked、显式 `CARGO_TARGET_DIR=/Users/StatTrak/Desktop/Switch_MAC/build/rust-target`。纯函数阶段先用 stub 得到 2 pass / 7 fail，实现在 green 后为 10/10；owner 阶段先得到 0/5，再为 5/5。日志：`build/reports/iteration-556-preflight-{red,green}.log`、`iteration-556-owner-gates-{red,green}.log`。

接线后 focused `cargo ... --bin switchd-rs quarantine` 冻结生产源码后最终完成 26/26（包含 reader 正向发送、完整 call-mode DTO、最后内部入口保护）；`build/reports/iteration-556-http-green.log`。这些测试只用合成存储、mock factory/channel 与匿名 Unix socket pair，没有物理枚举。546 sandbox guard 5/5：`iteration-556-sandbox-guards.log`。Go exit gate PASS：`iteration-556-go-exit-gate.log`。

contract gate 路由数量仍 294 Rust / 127 Swift / 54 HEAD，review scopes 从 430 到 431；因生产 hash 与 census 变化正确拒绝冻结状态。`iteration-556-contract-gate.log`。**未**自动重写 inventory，待独立复核后由主审统一冻结。

最终 focused 测试结束后通过 `ps` 核对已无 cargo/rustc 进程；误建的 src/rust/target 852 MiB 已由主审经 lsof 预检删除，后续均显式使用 build/rust-target。

独立复审后的构建与 555 复测结果见末节；完整 UI 来源、daemon SHA256、源码稳定性与重新 ad-hoc 签名、原始/复制 bundle inventory 均单独记录，没有覆盖 555 失败报告。旧 3.42 退出 1、真实模块恢复/读卡器/运营商互通、完整 SwiftUI 与未包装回退 marker 证明仍不在本次离线验收内。

## 建议独立 reviewer 优先核查

1. main startup 的预构造与四类后台是否全部在同一不可变 Gate 后；读卡器/N 待/ordinary IMS 构造与任务是否保持。
2. actor 的 Weak client 重建、USB 最低 factory、radio cleanup/fence、direct ADB probe/runtime/voice worker、两个 internal/shutdown entrypoint 是否都不能旁路。
3. public 路由分类是否遗漏物理能力或误伤 reader；shared SMS/call 控制是否先解析原 owner；备份改写是否在触达文件前拒绝。
4. no-owner finalizer 省略是否仅发生于隔离启动且未构造；accepted/unknown 与残留 owner 是否仍保持保守清理栅栏；exit 0 文义是否仅限本进程。
5. safe GET 是否完全无 AT/ADB/USB factory/恢复证据写入；module unavailable DTO 能否保留 snapshot reader。
6. pristine lineage 与完整 P6 的证据边界是否明确，且无基于空锁/无 backup 的恢复推断。

## 独立审阅 P1/P2 修复（2026-10-18，已重新冻结候选源码）

P1 已真实 red 复现：有效 marker 下，两个锁的非空/目录/symlink/hardlink 与未知 voice-runtime 都被旧判定整体跳过。`iteration-556-review-pristine-red.log` 为 8 pass / 3 fail。修复后每个锁仍逐项 lstat，仅 absent 或 regular、size=0、nlink=1 在有效 marker 下放行；不修改文件。

voice-runtime 不再由 marker 单独豁免。可放行的缓存必须只有当前嵌入 manifest 的版本目录、精确的 artifact 名称集合，以及可选 regular/size=0/nlink=1 的 `.download.lock`；版本目录和父目录不能是 symlink，artifact 必须 regular、nlink=1、精确 mode/size，再由既有 O_NOFOLLOW、pinned ancestry verifier 校验每个 SHA-256。未知版本、额外/缺少/部分文件、损坏、链接、异常下载锁都隔离。该检查确认完整的受控下载缓存形状，仍不证明历史物理设备恢复。**未经包装的旧版回退残留 marker 是独立未闭合 P6 blocker。**

正向单测用注入的微型合成 manifest 验证与生产相同的判定路径，无网络/运行时下载；生产 `inspect` 仍只能传嵌入的真实 manifest，不能用该合成 manifest 放行任意文件。正常新版本 ADB 只读空锁仍通过。没有改动模块业务或读卡器 owner 接线。

P2 已真实 red 复现：隔离 `POST /api/sms/adopt` 返回 200/adopt=true。`iteration-556-review-sms-adopt-red.log` 为 0 pass / 1 fail。修复仅将 POST 纳入模块路由门控；负例要求 409、原文件 bytes/inode/mtime/ctime 不变、不创建 JSON lock、live adoption 不变，GET 仍可只读。

此次生产差异仅 `module_recovery.rs`，另改 `module_quarantine_tests.rs` 与本文。最终 module_recovery 14/14 通过：`iteration-556-review-pristine-green.log`；从同一次已重建 test binary 运行 quarantine 组 28/28 通过：`iteration-556-review-quarantine-green.log`，包含 P2 文件/live 不变与 reader 正向 IMS 发送。所有 cargo/rustc 进程已退出。

25 文件源码指纹重新冻结；旧指纹另存 `iteration-556-source-snapshot-before-review.json`。本阶段没有更新冻结 inventory，复审后隔离 candidate 构建/受测结果见下节。

## 独立复审后的隔离 Release 与同目录验收

Reviewer 最终确认最新 P1/P2 无新 P0/P1 后，记录 938 个 Rust 输入文件指纹，使用明确的 `CARGO_TARGET_DIR=/Users/StatTrak/Desktop/Switch_MAC/build/rust-target`、Rust 1.98.0、offline/locked、MACOSX_DEPLOYMENT_TARGET=27.0 构建 Release daemon（2m57s）。没有构建或覆盖 dist/Applications，没有生成 DMG。

候选为 `/Users/StatTrak/Desktop/Switch_MAC/build/iteration-556-candidate/Switch.app`。UI 壳只读复制自 553 Release 输出 `build/DerivedData/Build/Products/Release/Switch.app`，版本 3.43/343，原始 bundle 的 29 个文件及 SwiftUI 可执行文件 SHA256 均记入报告。仅替换新的 daemon、将 libusb 引用重写为 @loader_path 后，分别对 daemon 与外层 bundle 重新 ad-hoc 签名；`codesign --verify --deep --strict` 通过。外层重签使 UI 主 Mach-O 的签名封装与 CodeResources 相应改变，UI 没有重编译或启动。

- 原始 Release daemon SHA256：`f65c0c84eef56e5de1772d8350a1cb706ea71079d711afb921b35bac7020fb3b`。
- 打包/实际受测 daemon SHA256：`e66efe25bd0768c16041c5517cfec3e8098b9f5e79f7ea486b3bf4359d2b09a9`。
- 构建、原始/最终 bundle 指纹、938 个源码指纹及最终比对：`build/reports/iteration-556-candidate-bundle.json`。
- 编译日志：`build/reports/iteration-556-candidate-build.log`。

用原 555 harness 的 opt-in `--expect-module-quarantine` 运行旧→新→旧→新，完整复制 bundle、同一合成数据根，**每次启动前均通过原 546 mandatory sandbox 的父/子 canary**。546 helper SHA256 仍为 `2c146dbc8c105dcaf0552f1161f4481c12d92082101395860cac1dc8bfdb5e8f`。kernel deny 覆盖 IOKit、Mach、设备内容、外部 Unix/IPv4 连接；只允许阶段根内的通信和写入，不能放宽 guard。没有启动 UI、实卡/代理访问或使用真实业务数据。

报告：`build/reports/iteration-556-inplace-bundle-roundtrip.json`；日志同名 `.log`；保留样本 `build/runtime/p555-imv1an95`。

| 阶段 | 归档数 | 退出 | SIGTERM 后耗时 | socket 残留 |
| --- | --- | --- | --- | --- |
| 新版干净根 control | 4→4 | 0 | 0.138s | 无 |
| 旧 3.42 首次写入 | 4→3 | 1 | 0.010s | 无 |
| 新版同根升级（旧锁、无 marker） | 3→2 | 0 | 0.001s | 无 |
| 旧 3.42 同根回退 | 2→1 | 1 | 0.004s | 无 |
| 新版再次同根升级 | 1→1 | 0 | 0.002s | 无 |

两次新候选均明确投影 `legacy_recovery_required`、`historical_recovery_confirmed=false`，health 200/ok=true/模块不可用，AT 请求在提交前 409，SMS devices 仍包含 reader。两份旧空锁 bytes、inode、mtime、ctime 保持不变，无新增 pristine、恢复推断或强杀。unknown send/N 待消息/operation 不重放，已确认消息与未知字段保留；四存储数据往返通过。原始 3.42 bundle 与 555 基线逐文件相同；原始 UI bundle、受测复制 bundle 与 938 个 Rust 输入在构建/验收后均未漂移。

最终 `data_roundtrip_passed=true`、`candidate_module_isolation_accepted=true`、`candidate_shutdown_accepted=true`；**报告整体 passed=false、full_p6_acceptance=false**，因为旧 3.42 自身仍两次退出 1，且历史物理恢复未确认。exit 0 只证明本次新版 owner/socket 排空成功。

所有本轮自有 sandbox 进程组、cargo/rustc 均已退出，安装 daemon 未停止或修改。源码冻结 inventory 仍由主审处理。本轮末可用空间约 598 GiB；共用缓存 debug/deps 45 GiB、debug/incremental 3.6 GiB、release 2.6 GiB；候选 bundle 74 MiB。保留旧版基线和合成样本，不擅自清除后续 gate 需要的共用缓存。

仍未闭合：未经包装旧版回退残留 marker、并发外部替换恢复文件的 TOCTOU 假设、真实模块恢复、实卡/运营商互通、完整 SwiftUI 与性能验收。本轮离线结果不覆盖这些范围。
