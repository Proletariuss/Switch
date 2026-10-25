# Iteration 666 当前源码刷新复核

## 结论

Iteration 666 在完整迁移门禁修复稳定后重新冻结当前软件验收元数据。当前 reviewed set 为 **503 个文件**：不可变 i665 前置集合 237 个文件，加上 i666 精确增量集合 266 个文件；其中 79 个前置文件当前字节发生已审变化。最新增量纳入 Darwin listener claim 显式解锁生产修复及确定性回归，并刷新 checker 当前行。

本轮只刷新来源、入口、runtime owner/format/exit 和 P0 QMI 静态责任链。49 个父项、115 个需求叶子、21 类场景及其验收状态保持不变；新增功能 PASS 为 0，整体验收分母未冻结，产品完成状态为 false。

直接用户指令要求不连接设备。DJI USB/QMI/AT、PC/SC/BLE/eSIM、运营商 VoWiFi/短信/通话、物理音频权限/质量/长稳、Telegram/Bark 外部投递及旧 3.42 降级支持六组均为 `UNVERIFIED_EXCLUDED_BY_USER`，没有转为 N/A、PASS 或产品完成证据。

## 503 文件集合

- i665 前置集合：237 文件；SHA-256 `a60ad2f488991159038ed51f8b86b71a0541afd14d07525bcd1a4a6c6d09e5be`。
- 首次 i666 人工审阅增量：248 文件，来源为 checker 集合、冻结的 62 份专项报告和两个强制无设备验收脚本的去重并集。
- 门禁修复后新增：13 文件；来自 SMS/HTTP fixture、VoWiFi production-integration fixture 及 runtime fixture/lifecycle 修复报告。
- 后续 test-only fixture 新增固定：1 文件；`native_recovery_tests.rs` 加入稳定 `client_request_id`、`mutation_receipt` 断言及 clean/SIGKILL exact replay。
- 冷胶囊 listener-lock 生产/回归新增固定：1 文件；`socket_lifetime.rs` 显式解除 fork 继承的锁描述，并加入确定性重复描述回归。
- 相对 i665 当前变化：79 个前置文件内容改变；当前新增集合 266 文件；缺失文件 0。
- 当前快照：`docs/optimization/all-rust/iteration-666-source-snapshot.json`，503 文件，SHA-256 `46247c468f7c2a7174a3f42d752c184d7e8b12ca3f4456cc0acaa01cdb7a281d`。

13 个新增文件：

- `rust/crates/proletariusd/src/device_sms_send/tests.rs`
- `rust/crates/proletariusd/src/module_radio/bootstrap_projection_tests.rs`
- `rust/crates/proletariusd/src/native_vowifi_session/service/production_integration.rs`
- `rust/crates/proletariusd/src/native_vowifi_session/service/production_integration/{call_exchange,gateway,module_exchange_serial,refresh_exchange,rekey_exchange,sessions_exchange,sms_exchange}.rs`
- `rust/crates/proletariusd/src/reader_pcsc/tests.rs`
- `rust/crates/proletariusd/src/serial_esim_gateway_tests.rs`
- `rust/crates/proletariusd/src/serial_esim_gateway_tests/download.rs`


## 最终契约元数据冻结

独立人工审查报告 `build/reports/iteration-666-final-contract-fingerprint-review.md`（SHA-256 `978e4fb64100cabcc2214dfc98c8df7842c227b3f5c479c02807c6839062c4c3`）确认五个 normalized scope 漂移均来自已审门禁修复：`modem_actor.rs` 的 test-only opener error、`public_http.rs` 移除两个 Rust-only path 的 legacy mux 误归类、`reader_esim_download.rs` 的 selection-first 顺序，以及 `reader_pcsc.rs` 的 Weak worker 生命周期。四个生产源码 raw hash 在写入前与报告完全一致。

`api-contract-review.json` 只更新这五个 fingerprint 并追加审查语义；随后仅执行一次受控 `--write-inventory`。最终 review SHA-256 `c399041ccb6f5a354a3bd9dc434e28da0b7cb108d640a9cc27915c91ed6159c7`，inventory SHA-256 `2e8a1abf1759195f96ba0e7720d08ce7127e3e6db012c68cf491abf0c09e9fde`。同步后的无写 checker 与 `--diff` 均通过，计数保持 304 routes / 304 Rust / 0 Go / 125 Swift HTTP / 470 scopes / 57 HEAD；没有新增 route、owner、DTO、认证或 dispatch 权限。

最终契约元数据冻结后，`module.rs` 修复 stop/register TOCTOU，`module/tests.rs` 增加确定性回归覆盖；两者在当时既有的 499 文件集合中只刷新对应行。route/owner/DTO/认证/dispatch 与验收边界不变。入口映射中 14 条 modem internal records 按稳定 `entry_id` 重算 original-record digest，并把 `original_sources` 与 `owner.catalog_source_refs` 从旧行 4490 重绑至 current inventory 行 4498；没有按旧 index 推断语义，也没有改变 route/owner/handler/note、leaf、场景、实例或 PASS。


## Native recovery test-only fixture 刷新

`rust/crates/proletariusd/tests/native_recovery_tests.rs` 当前 SHA-256 为 `838ba22e68d4633bd4c7f81b8a33db244f9b3d0c7ae86d2bd3d29f2920245e86`。修复只为 delete-sender fixture 增加合法且稳定的 `client_request_id`，校验 `mutation_receipt`，并让 clean restart 与 SIGKILL recovery 精确重放同一请求。证据报告 `build/reports/iteration-666-native-recovery-fixture-fix.md`（SHA-256 `80057a557d8dbd1a9e1f94d66605273d81cadfaf867d02f2807c42ec92ca71ad`）记录 exact 3/3 和相关 51/51 通过。

生产代码、route、DTO、认证、owner、contract review/inventory、runtime source anchor 和 QMI source/anchor 均未改变；本项不产生功能 PASS、产品完成或分母冻结。

## 契约与入口映射

契约清单保持 304 routes、304 Rust、0 Go、125 Swift HTTP、470 scopes、57 HEAD。本次在独立人工审查五个 scope 后执行一次受控 inventory 同步；同步前无写 checker 仅剩预期 inventory diff，同步后无写与 `--diff` 均通过。入口映射按稳定 route identity 重绑 14 条仅行号变化的 modem internal records，并继续按 Swift `(file, normalized expression, routes)` 多重集，而非旧数组索引：API 295 保留 + 9 新增 = 304；Swift 114 保留、20 移除、13 新增 = 127。

门禁修复后刷新了 `modem_actor.rs`、`public_http.rs`、`reader_esim_download.rs`、`reader_pcsc.rs` 和 acceptance checker 的当前 source pin；当前 source pin、source fragment 与目录映射校验均为 0 错误。当前入口映射 SHA-256：`0ab2b72a098ed3c05e88aacfce4b609aa37fe3e25e0381d3eee4ef157486d9f9`。

## Runtime owner/format/exit

342 条历史 source record 原文保持不变。当前 disposition 仍为：182 `UNIQUE_EXACT_FRAGMENT_REBOUND`、111 `EXACT_WHOLE_FILE_RAW_MATCH`、49 `SUPERSEDED_OR_REANCHOR_REQUIRED`；活动 current successor 75，归档 superseded successor 6。

门禁修复实际要求 15 个 historical current binding 和 5 个 successor 重绑：`reader_pcsc.rs` 5 个、`reader_esim_download.rs` 7 个、`modem_actor.rs` 3 个 historical + 5 个 successor。Reader 两组 exact fragment 仍唯一，只更新当前 hash/line/snapshot inventory。

`modem_actor.rs` 没有仅替换 hash。人工采用稳定唯一上下文：

- historical actor thread：882–884，`Builder + name + spawn` 三行；
- historical shutdown：1614–1618，函数签名和四行 teardown 说明；
- current terminal shutdown successor：1614–1672 完整函数；
- historical close branch：2635–2638；
- 其余四个 successor：882–905、2030–2038、3812–3822、3881–3884。

这样 historical shutdown 与 successor 不再使用相同 exact fragment；每个 current fragment 的唯一匹配清单均重新计算并通过。Runtime 完整校验错误为 0。当前 runtime map SHA-256：`34df60587fc66f827a531dfef9472c2757609732c2b68b99b586c0537baae309`。

## P0 QMI 静态责任链

九文件、59 anchor 的 P0 QMI trace 在当前生产源码中没有语义或行锚点漂移；validator 为 0 错误。当前只将其绑定到 503 文件快照并更新 active snapshot pin；新增与刷新的 test-only fixture 均不属于 QMI source/anchor。该证据仍不证明真实 DJI 枚举、CTL_SYNC、USB/QMI 响应、SIM/IMS 注册或物理退出。当前 trace SHA-256：`cad2aed92e62075ddc551dd284bab0545c24df74248ec9f7ef297c3bb86ddfad`。


## Module stop/register TOCTOU 刷新

`rust/crates/proletariusd/src/native_vowifi_session/module.rs` 当前 SHA-256 `4118ef393591396bcf598893ada4f8787dfea363308a8cd2f232fe30a46b0645`，`rust/crates/proletariusd/src/native_vowifi_session/module/tests.rs` 当前 SHA-256 `679f63eaa40bb52e50e0ffaaae2e8671164180579d080646fda2a4ce93227a77`。Registered 发布现在持有 `RecordState` 锁并重新检查 `operation_done`、`stopping` 与 stop watch，阻止晚到的注册成功在 RF 恢复之前提前发布操作完成。确定性 disable/shutdown 回归 2/2、production-composition 集成 1/1、压力回放 40/40 均通过；证据报告 `build/reports/iteration-666-module-stop-register-race-fix.md`（SHA-256 `b891404208be7f70783625a0e2b6767de3ad5a2bbc689b941c4fcfeaf2ab16c2`）。

同一受控刷新把 contract review 更新为 `c399041ccb6f5a354a3bd9dc434e28da0b7cb108d640a9cc27915c91ed6159c7`、inventory 更新为 `2e8a1abf1759195f96ba0e7720d08ce7127e3e6db012c68cf491abf0c09e9fde`。无写 checker 与 `--diff` 均通过，计数保持 304 routes / 304 Rust / 0 Go / 125 Swift HTTP / 470 scopes / 57 HEAD。只有三条 VoWiFi route 的 source line 从 845 移至 852；route、method、owner、Swift call、scope、HEAD alias 和验收状态均未变化。Runtime 的 `module_vowifi_launch` 与 `module_vowifi_join` exact fragments 仍各唯一，当前锚点分别为 578–596 与 217–256。

本项不产生设备、运营商、外部投递、物理音频、旧版回退、功能 PASS、产品完成或分母冻结。

## 软件验证与边界

门禁修复报告记录完整迁移门禁在 gate-fix 源码身份上通过；本元数据任务只复跑 acceptance ledger 测试、ledger checker、source identity 和 maps/ledger 自检，不重新运行冷构建或生成 DMG。硬件、运营商、外部投递、物理音频、长稳和旧版回退仍需独立验收。

主要输入证据：

- `build/reports/iteration-666-gate-runtime-fixes.md`
- `build/reports/iteration-666-vowifi-integration-token-fixture-fix.md`
- `build/reports/iteration-666-gate-sms-http-fixes.md`
- `build/reports/iteration-666-full-migration-gates-after-fixes.log`


## 门禁 Shell 调用可移植性最终刷新

`scripts/test-migration-gates.sh` 当前 SHA-256 `eaa1f78362599b91a6e9fc209e3150882c9f7f27c816298310d6ceac08a01049`。三个保持 0644 的 POSIX shell 测试脚本现由门禁显式使用 `sh` 调用，避免源码导出不保留执行位时在完整门禁后段出现 `Permission denied`。三个目标脚本的定向回归、语法检查、递归可达脚本权限审计与 `git diff --check` 均通过；证据为 `build/reports/iteration-666-gate-shell-invocation-portability.md`。

该阶段的 499 文件集合没有增删，只刷新上述既有脚本行；API route、owner、DTO、认证、dispatch authority、runtime owner/format/exit、P0 QMI 九文件/59 anchors、功能 PASS、验收分母及产品状态均未改变。六组用户排除项继续保持 `UNVERIFIED_EXCLUDED_BY_USER`。该阶段 snapshot SHA-256 为 `334d584dab46bbfd4e426cdc9b63f70cb2062e91f3cfadc5c4b7aca77e1ab065`。


## 冷胶囊 SUN_LEN 测试夹具最终刷新

冷胶囊首次执行暴露 Darwin AF_UNIX `SUN_LEN` 路径上限：五份 test-only fixture 直接从长源码/构建目录创建 socket。当前修复只把瞬态 socket 移到系统短临时目录；持久测试数据、Cargo 构建和生产路径保持原有隔离。`reader_lifecycle`、`public_http`、notification runtime、VoWiFi gateway 和 sessions exchange 的定向与长根路径回归均由三份证据记录。

该阶段受控来源刷新将 `reader_lifecycle/tests.rs` 与 `public_http/tests.rs` 首次加入快照，刷新 notification/gateway/sessions 既有行，并同步 checker 声明；当时为 501 文件、79 个相对 i665 changed、264 个 added。该阶段 snapshot SHA-256 `deb4d3512cf9d6037ef5971c18763c8d0b3f589ed26a55cbefc4e453b93cfbdc`，entry map `2ebc1893972e69a231337f3f86fe93f273f10d2d5fda2c56be6323a00e84e75c`，runtime map `f3ff43e658b9db43f1b0c2e61a502e35b9bddfd45861a1daab7dfa9f7de889d1`，P0 QMI trace `ad06a7380487e913aa2bb37d306e96051b4013e58bceb1253681dfad6ea0e554`。历史阶段哈希、contract review/inventory、49 个父项、115 个需求叶子、21 类场景、功能 PASS、分母和产品状态均未改变；六组用户排除项仍为 `UNVERIFIED_EXCLUDED_BY_USER`。


## Native process/recovery SUN_LEN 夹具刷新

冷胶囊完成 3270 项 library 测试后，五项 `native_process_tests` 因公开 AF_UNIX socket 仍位于超长 `CARGO_MANIFEST_DIR` 派生目录而在 readiness 前失败；三项 `native_recovery_tests` 具有相同潜在缺口。两组 fixture 现在使用 `std::env::temp_dir()` 下碰撞安全、权限 `0700` 的短私有根目录，socket、日志、数据与 PID 文件仍由同一 fixture 生命周期持有；生产进程路径与语义未变。正常目标测试 8/8、163 字节长根路径下 CLI/process/recovery 11/11、Rust fmt 与 diff check 均通过。

本阶段首次固定 `rust/crates/proletariusd/tests/native_process_tests.rs`，刷新 `rust/crates/proletariusd/tests/native_recovery_tests.rs` 既有 added 行，并同步 checker；最终为 502 文件、79 changed、265 added。当前 snapshot `7101f8781cd50e8eb8db3cac81c68cb39801d1403264272dc399d31790209577`，entry `0e76990a953e8bc20eed856ecb8f3e844d20f4e17840501ddde77077204d21b7`，runtime `da4ef7edbe75071a93cd280abcbfaea918fcf01ca4497d2727a5eae36b722f80`，QMI `69162b8a3880abb933a338f0d5f8a5986dfb10e022496d3386e81260ea18d303`。contract inventory/review、49 个父项、115 个需求叶子、21 类场景、功能 PASS、分母、产品状态与六组 `UNVERIFIED_EXCLUDED_BY_USER` 排除项均未改变。


## Darwin fork 继承 listener lock 最终刷新

冷胶囊在 3269/1/8 时暴露 listener claim 仅依赖文件描述符析构：Darwin 子进程在 fork 与 exec 之间可继承 `flock`，使原 owner drop 后仍瞬时 `Busy`。`rust/crates/proletariusd/src/socket_lifetime.rs` 现于 `Drop` 中显式 unlock，并用仍存活的重复 open-file description 确定性验证立即重取。exact regression 1/1、完整 socket suite 18/18、Rust fmt 均通过。

本阶段将 `rust/crates/proletariusd/src/socket_lifetime.rs` 首次固定并刷新 checker；最终为 503 文件、79 changed、266 added。当前 snapshot `5a9f9d3c6dfa8d835c4acced5dbaf1b1079b0957f7b67e665638b012235cc25f`，entry `7f392ec899867449b8bb1c9837f2f65847bf2e4a2d8e2f10189d5e4241b6006a`，runtime `ffc9d49c0966fb14edc02dd4a31ef651a49599e2066c166e0a5b466ac29b2b49`，QMI `1f170ffe619f21a9690e1bd2123f2793de52d25041242692f124f0d3d4df2ed1`。证据 `build/reports/iteration-666-socket-lock-fork-release-final.md`（`e02f3cd51012d5dd1a4d2764585baf92cc26cc7d91acabd810b901ef168a0dcd`）及 `build/reports/iteration-666-socket-lock-fork-release-final-targeted.log`（`ebafd66f39763932aca8f2fdf32f0dbfd5d1f4fdcc7895edf3443ff1d84c9915`）。API route/owner/DTO/认证/dispatch、runtime catalog 节点、QMI 九文件/59 anchors、功能 PASS、分母与产品状态均未改变；六组排除项继续为 `UNVERIFIED_EXCLUDED_BY_USER`。


## Acceptance ledger 跨组 source pin 负向回归刷新

`scripts/tests/test_acceptance_ledger.py` 的跨组冲突测试此前错误依赖 supplementary pin 顺序；503 文件集合扩展后首项不再属于 current pin 组，因此只会得到正确的 `source pin drift`。测试现先选择两个 review group 实际共享的文件，再篡改 supplementary identity，目标回归 1/1 通过。生产 validator、API、runtime owner/format/exit、QMI 九文件/59 anchors、需求与场景均未改变。

当前 snapshot `46247c468f7c2a7174a3f42d752c184d7e8b12ca3f4456cc0acaa01cdb7a281d`，entry `0ab2b72a098ed3c05e88aacfce4b609aa37fe3e25e0381d3eee4ef157486d9f9`，runtime `34df60587fc66f827a531dfef9472c2757609732c2b68b99b586c0537baae309`，QMI `cad2aed92e62075ddc551dd284bab0545c24df74248ec9f7ef297c3bb86ddfad`。证据 `build/reports/iteration-666-ledger-cross-group-pin-fix.md`（`b50731f2613f911af9188c1ce8734daa72f486845627013893f430d276e0b61d`）和 `build/reports/iteration-666-ledger-cross-group-pin-targeted.log`（`25731833f9409651c24426b70de0f6929e729bd36ae02e81a30e9ce0a804794f`）。这是 test-only provenance refresh，不产生功能 PASS、分母冻结或产品完成；六组用户排除项继续为 `UNVERIFIED_EXCLUDED_BY_USER`。
