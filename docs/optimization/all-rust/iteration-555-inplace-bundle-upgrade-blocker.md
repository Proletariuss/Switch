# Iteration 555：复制 bundle、同数据目录升级/回退的退出阻塞

2026-10-18。P6 的“安装升级：旧版本脱敏数据升级、退出/重启、回退副本”
仍有缺口。546 与 549 只在新目录间复制指定业务文件，特意排除了旧锁与其他
运行痕迹。本轮新增 opt-in `scripts/check-p6-inplace-bundle-roundtrip.py`，保留
同一合成数据目录中的完整运行痕迹，验证实际旧/新 bundle 内的 daemon。

## 输入与隔离

完整复制旧 3.42/build342 bundle 和既有 Iteration 541 的 3.43/build343
bundle 到 `build/runtime/p555-*/bundles/`，逐文件核对 SHA-256、权限与内部
符号链接，并对副本执行严格签名验证。整个测试之后再次核对原 bundle 与
副本不变。3.43 输入明确是 541 留存快照，其后端 SHA-256 为
`29cd95a16a905afc095ea55444caeb59aa7b9f13f7637cedcdf1bad4932f7933`；
不是本轮 IKE DEVICE_IDENTITY 修改后的构建，没有据此宣称最新主源码已验收。

所有数据来自 546 的四份字面合成 fixture。先在独立干净目录运行 3.43 对照，
再对另一个共享合成目录按 3.42 → 3.43 → 3.42 → 3.43 顺序运行。首次 seed
后不复制或重建该数据目录，不删除、改写或重新分类生命周期锁与标记。
只执行业务读取和三个合成归档删除；不调用通话模式、模块或 SIM 操作 API。

每个进程强制使用 546 的硬件/外网禁止沙箱，并在启动前验证父子进程继承。
没有启动 SwiftUI，没有安装或覆盖 `/Applications/Switch.app`，没有读用户
数据、Keychain，未接触真实卡片、代理或已安装 daemon，没有生成 DMG。

## 已确认结果与失败判定

干净 3.43 对照能够读取全部合成数据，SIGTERM 零退出，原生退出回执存在，
socket 全部移除，目录建立了 `.call-mode-pristine-v1`。

旧 3.42 在另一个全新合成目录开始时没有生命周期文件，完成读取/归档删除
及退出之后，实际生成了两个零字节文件：`call-mode-operation.lock` 与
`.module-adb-interface.lock`。文件来自这一实际旧二进制运行，非脚本预置；
当时还没有启动候选 3.43。没有 pristine 标记、模块备份或 voice-runtime。
旧版自身仍以通话模式清理错误退出 1。

随后 3.43 在同一个目录成功读取旧版写入，确认归档删除，未知发送仍不可
提交，但 SIGTERM 后停留在通话模式清理。日志包含：

```text
switchd-rs shutdown: call mode: 未找到支持的模块 USB ADB 接口；停止前路由状态未确认：未找到支持的模块 USB ADB 接口
```

测试给予每次退出 85 秒；超时只终止测试自己建立的进程组，并将
`shutdown_timeout=true`、`forced_owned_group_cleanup=true`、退出码与残留
socket 记入报告。强制结束绝不视为正常退出。随后仍保留该目录和旧锁继续
回退检查；进程组清理失败或数据断言失败会中断测试。脚本分别报告数据兼容
与退出接受结果，只有旧/新退出全部通过才会令顶层 `passed=true`。

完整证据为 `build/reports/iteration-555-inplace-bundle-roundtrip.json`；相对
`Switch_MAC` 根目录。早期运行首次遇到退出超时便中止，其部分报告保留在
`iteration-555-inplace-bundle-roundtrip-snapshot541.json`，没有作为成功结果。

| 共享目录阶段 | 归档条数前 → 后 | 退出结果 |
| --- | --- | --- |
| 旧 3.42 首次运行 | 4 → 3 | 1，旧通话模式清理错误 |
| 3.43 快照原位升级 | 3 → 2 | 超过 85 秒，测试清理后 -9 |
| 旧 3.42 原位回退 | 2 → 1 | 1，同一旧版错误 |
| 3.43 快照再次升级 | 1 → 1 | 超过 85 秒，测试清理后 -9 |

报告为 `validation_completed=true`、`data_roundtrip_passed=true`、
`candidate_shutdown_accepted=false`、`legacy_shutdown_accepted=false`、
`passed=false`。八次数据检查均保留确认正文、中文/emoji 扩展字段、未知
发送不可提交、N待及操作恢复状态；原始 fixture 副本哈希不变。两次候选被
测试清理后留下陈旧 socket，报告明确记录；这不等于后台进程仍存活。
最终所有测试自有进程组均已结束，未清理共享目录内的锁或最终陈旧 socket。
五项强制沙箱边界正/负例测试再次通过，脚本编译检查与差异空白检查通过。

## 只读源码定位

- 当前 `call_mode_lifecycle.rs:48` 的 `initialize_pristine_marker` 只有在历史
  call-mode 锁、ADB 锁、备份与 voice-runtime 全部不存在时才创建证据。
- 同文件 `requires_runtime_cleanup`（约 1391 行）对无标记旧目录中的锁继续
  保守判定需要清理。零字节、未发现设备、无备份都不是物理恢复证明。
- `finish_shutdown_receipt`（约 1345 行）缓存第一次最终清理结果。错误回执
  后续继续返回错误，避免轮询盲目重放物理操作。
- `shutdown.rs:73` 的 `confirmed` 每秒重取回执，只有成功才向下执行。
  因而已缓存的未知错误会持续阻塞。不能把删除重试循环或忽略错误当作修复。
- 归档旧实现的 `call_mode.go` 与 `adb_interface_lock.go` 使用 `O_CREAT`
  创建零字节锁；释放时只解锁、关闭，不删除文件。此归档源码是 2.97 参考，
  不假称为 3.42 二进制的精确源码；本轮实际 3.42 前后文件快照独立确认了
  旧运行生成两锁这一事实。未采集系统调用跟踪，因此不声称已精确归因到
  某次旧 Rust/Go 读探测或退出函数。

## 待评审的最小安全方案：先隔离历史不明状态，再显式恢复

以下是方案，没有修改生产源码、API 或锁策略。应在 IKE 主源码门禁完成后
单独实现和审阅。

1. **启动前置判定。** 新建纯文件证据检查器，区分 `ready` 与
   `legacy_cleanup_required`。旧锁/损坏标记/不可读证据均保留阻塞；不凭锁
   大小、版本号、无备份或缓存 disconnected 状态写入 pristine。
2. **不启动物理 owner 的隔离模式。** 对历史不明目录，在 module actor、
   radio、call-mode、voice-route 等物理 owner 接受任何工作前进入只读隔离。
   短信历史等纯数据接口仍可用，新增物理操作返回 409
   `legacy_call_mode_recovery_required`。只有确定本次没有建立物理 owner、
   没有接受操作时，隔离进程自身的退出才可正常完成；迁移 readiness 仍为
   false。已有活动/未知物理操作的正常运行分支保持现有清理栅栏。
3. **只读诊断契约。** 在现有 `GET /api/v2/runtime` 追加兼容的 `migration`
   对象：`state`、`reason_codes`、`proof_epoch`、`blocked_admissions`、
   `recovery_operation_id`。不返回用户路径/设备秘密，不把读诊断变为硬件
   探测。`GET /api/call-mode/status` 只投影该判定，不自动部署或恢复模块。
4. **显式恢复入口。** 可新增独立的
   `POST /api/call-mode/migration/reconcile`，严格校验 `confirm=true`、选定
   模块的稳定身份/代次、有效 backup_id 与幂等 operation_id，再交给现有
   资源仲裁与恢复执行器。不存在设备、身份不匹配、无有效备份或未知提交
   均不产生“已清理”证明。不要复用忽略 body 内容的旧 `/api/call-mode/retry`
   来承载新确认语义。没有备份的历史不明情况保持需人工恢复，不自动生成
   备份或清空证据冒充回退。
5. **证明的迁移规则。** 允许 pristine 的依据必须是在任何旧代码运行前
   建立并持续保持的可信无物理修改来源，或选定模块实际恢复/关闭得到确认
   后同步落盘的回执。进入不识别该协议的旧版前，受控回退启动器必须先撤销
   pristine 并 fsync，再允许 exec；撤销失败就拒绝旧版启动。未经包装启动的
   历史旧版不能用新 marker 的存在自动证明安全。只读隔离模式不能自行给旧
   目录补 marker。

最小离线测试范围应覆盖：两空锁/无备份仍隔离；缺失/损坏/符号链接证据
不被当作 pristine；隔离状态零物理调用、正常退出且 readiness=false；已接受
或未知操作不走快速退出；缺设备、身份错配与无备份的显式恢复均无写入；
确认回执落盘失败仍阻塞；回退启动前撤销失败则不 exec；重复 operation_id
不重放；设备重连只允许重新协调未提交阶段，未知提交不盲重试。成功物理
恢复可先用受控执行器模拟，但仍须另做选定模块实机验收。

## 复现与边界

```sh
python3 scripts/check-p6-inplace-bundle-roundtrip.py \
  --legacy-app ../build/release-3.42/Build/Products/Release/Switch.app \
  --candidate-app ../build/iteration-541-derived/Build/Products/Debug/Switch.app \
  --candidate-label iteration-541-existing-3.43-build343-snapshot \
  --report ../build/reports/iteration-555-inplace-bundle-roundtrip.json
```

本轮是留存 3.43 bundle 的隔离失败验收及只读原因定位，不是正式安装、完整
原位升级成功或最新 IKE 源码验收。最新二进制构建应等待主源码冻结复核后
再复测；本报告不提高整体完成百分比，也不将 P6 标记完成。
