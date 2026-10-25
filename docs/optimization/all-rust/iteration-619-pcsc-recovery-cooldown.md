# Iteration 619：PC/SC恢复冷却反馈与P6冷构建复核

日期：2026-10-21。此工作包继续修复读卡器生命周期缺口，并验证 Rust-only 3.46 候选。P6 往返数据为合成数据，应用副本在拒绝硬件与外部网络的沙箱中运行；另行使用冷构建 Rust daemon 对 ESTKme-RED / SIM World 做只读连接、EID/Profile 概要和 VoWiFi 注册探针。本轮没有切换/下载 Profile、发送短信、拨号、安装应用或生成 DMG。

## PC/SC恢复状态

`rust/crates/proletariusd/src/reader_pcsc.rs` 在一次卡片无响应后原本会将再次获取请求映射为通用的无响应结果。冷却期仍会阻止连续复位，但调用方无法判断读卡器正在恢复，也无法知道何时重试。

现在 PC/SC owner 对复位后的5秒窗口返回 `reader_pcsc_recovery_cooldown`：HTTP 503，中文提示向上取整的剩余秒数；`submitted=false` 且 `uncertain=false`，表明操作尚未提交、不会因等待状态误报为结果未知。冷却期内不会重复复位；冷却期结束后可再次尝试。新增测试覆盖冷却时间边界、结构化错误字段，以及同一 owner 连续读卡失败时只执行一次复位。

验证：`cargo fmt --all -- --check` 通过；冷却用例1项通过；`reader_pcsc::tests::` 48项通过。

## 迁移门禁和冷构建

完整 `scripts/test-migration-gates.sh` 在本轮 PC/SC 改动后退出0：Rust 75个测试目标合计3932通过、0失败、9忽略；Python 9组共100项通过；冻结协议回放297次、无Go执行；BLE生产事件序列41组通过；接口契约294/294归Rust、Go运行时为0。日志保留在 [`iteration-619-full-migration-gates-final.log`](../../../../build/reports/iteration-619-full-migration-gates-final.log)。

随后从隔离源码胶囊完成3.46/build346 arm64冷构建。`audit-clean-build verify`、artifact audit 与 `codesign --verify --deep --strict` 均通过。包内的业务可执行文件为Swift App和`switchd-rs`，另有`libusb`动态库；未打包Go daemon。冷构建的源码指纹为 `7529969838427e52530db4a3fc2391eed90132405ed626cb8541d76ab214bc23`。候选位于 `build/clean-build/iteration-619-b12-cold-build/dist/Switch.app`。

复核发现 `scripts/audit-clean-build.py verify` 计算源码身份时漏传胶囊 `src` 为工作目录，可能将当前工作树指纹错误地与隔离胶囊中的嵌入指纹比较。验证函数现在显式指定 `cwd=capsule/src`，并在 self-test 中新增子进程工作目录回归；审计器自测通过，随后对冷胶囊重跑 `verify` 通过。

冷构建之后，本工作包还修改了 `scripts/clean-generated.rb` 及其专项测试，使清理器忽略明确的 `xcodebuild -runFirstLaunch` 工具初始化进程，同时仍阻止真实Xcode构建、Cargo或Rust编译进程。Ruby语法检查、清理器精确白名单/符号链接/活动构建保护测试、`git diff --check` 均通过。两个工具改动不改变App实现，但构建指纹已不代表最终完整源码；发行候选必须从届时冻结的源码重新构建。

## P6合成原位往返

用3.42/build342旧副本和本轮隔离冷构建的3.46/build346副本，在同一合成数据根目录运行四阶段升级/回退。数据读写、未识别字段保留以及新候选的模块恢复隔离均通过；新版升级和再次升级均在15秒内退出0、写出Rust shutdown receipt并移除socket。3.42旧版首次运行和回退均无法在硬件被沙箱拒绝时确认USB ADB接口，因此以 `call_mode_shutdown_unconfirmed` 退出1且没有shutdown receipt。完整P6报告因此为失败；没有把旧版失败隐藏为通过，也没有伪造硬件设备来使旧二进制通过。

报告：[P6 JSON](../../../../build/reports/iteration-619-p6-current-cold-candidate-roundtrip-final.json)。此项证明新版候选面对遗留模块状态时采取隔离策略并能干净退出；不证明旧版回退生命周期或实体模块恢复通过。

另外，使用同一冷构建胶囊内的 Rust daemon 对实机 ESTKme-RED / SIM World（454/03）进行了受限 EID/Profile 与 VoWiFi 复验。报告仅记录数量和状态：EID 1组、Profile 1组且读取状态完整，共10个Profile。VoWiFi 状态依次为 `connecting`、`connected/registering`、`connected/registered`；`sms_ready` 和 `call_ready` 均为 true。停止 VoWiFi、断开读卡器、daemon 退出及隔离数据目录删除均确认成功。该状态不是短信送达或电话接通证明；本次没有切换Profile、收发短信或拨号。脱敏探针新增概要测试3/3通过。报告：[SIM World EID/Profile/VoWiFi JSON](../../../../build/reports/iteration-619-simworld-eid-profile-vowifi-probe.json)，SHA-256 `04bddfb61f32e22f669c3e4dd137eda8895468c711b2cd03074d5d61b831e74c`。用于绑定本次运行的 candidate daemon SHA-256 为 `07fd451547759162d0941c0a38bd48a1ff6d5da8d4cd957e6f7a71373438534c`，来源于同一冷构建的 artifact audit；其嵌入源码指纹为上文所列 `752996...`。

## 缓存与整体验收状态

先确认没有进程打开所选缓存文件，再使用 `scripts/clean-generated.rb --remove` 精确移除9个可重建路径，合计回收18,600,775,680分配字节（约17.32 GiB）。保留隔离源码、依赖缓存、构建日志、审计与P6报告、当前 `.app` 候选和正式 `dist`。

PC/SC源码pin、5个生命周期历史锚点和当前源码快照经人工核对更新；`check-acceptance-ledger.py` 现报告 `metadata_consistent=true`，但功能验收计数仍为0 PASS / 5 PARTIAL / 110 NOT_REVIEWED，分母未冻结，因此不报告项目百分比。总体阶段仍为P0未闭合、P1–P4验收中、P5实网通话听音待用户配合、P6未收口。接口归属为294/294，不等同于整体完成。三轮交叉审议仅在全部代码与适用功能验收后启动；本轮没有生成DMG。
