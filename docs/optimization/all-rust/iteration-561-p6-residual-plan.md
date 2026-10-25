# Iteration 561：P6 旧回退与 pristine 证明残余

2026-10-18。结论：旧 3.42 的退出 1 是**模块语音路由清理无法确认**，不能改写为成功；556 解决的是新进程在模块隔离状态下的退出。现有源码也不能仅靠持久化 pristine marker 安全识别未经包装的旧版回退，两项仍阻塞完整 P6。

## 已确认的失败原因

555、556 合成报告中，旧版首次运行及回退的四段日志均在 SIGTERM 后终止于同一错误：

```text
switchd-rs: I/O failure: call mode shutdown unconfirmed: 未找到支持的模块 USB ADB 接口；停止前路由状态未确认：未找到支持的模块 USB ADB 接口
```

556 的两次旧进程分别在 0.010s、0.004s 内退出 1，无 timeout、强杀或 socket 残留；此前业务数据读写均通过。故不是数据往返失败或 555 新版曾出现的无限等待。强制 sandbox 中没有可访问的目标模块 ADB 接口，旧清理路径不能确认原语音路由是否已停止，错误最终传播为 Rust 进程退出失败。**接口不可访问不能证明设备原本已恢复**，也不能从该日志区分“无兼容模块”与隔离导致的不可访问。

受测旧 Rust SHA256 为 `1884504d65ada5ee3ba344969e6ea4b80b045ba9d73f8301ff05937d641b427d`，与两轮 report 的 bundle inventory 一致。只读字节检查在该二进制中找到上述三个错误片段及两个 lock 文件名，未找到 `.call-mode-pristine-v1`；配套 Go 二进制未找到这些片段。可以定位失败出口为旧 Rust 的 call-mode shutdown；字符串缺失不是完整反汇编证明。

现有 `call_mode_adb.rs` 的 `quiesce_preflight` / `finish_quiesce` 展示同一保守策略：status/root 失败后尝试停止，停止仍失败则组合“停止前路由状态未确认”。这是实现解释，**不是已核对受测 3.42 编译源码的逐行证明**。此次可找到的归档树 VERSION=2.96、Git HEAD VERSION=2.93；其中 Go 锁实现 O_CREAT 后仅解锁/close，不删除，是锁语义参考，不能冒称确切 3.42 源码。修复旧二进制前仍需找回匹配源码/构建清单。

证据：`build/reports/iteration-{555,556}-inplace-bundle-roundtrip.json` 及其指向的 `legacy_first_write.log`、`legacy_inplace_rollback.log`；原始 bundle、日志和合成数据未修改，也未启动任何旧进程。

## 为什么现有 marker 不能闭合未包装回退

`module_recovery.rs:143` 仍允许“有效 marker + regular/size=0/nlink=1 的锁”；`call_mode_lifecycle.rs:1445` 在本进程无已知物理工作、无 backup 时可据 marker 省略 ADB 清理。P1 已挡住畸形锁和未知 runtime，但没有证明 marker 创建后不存在旧进程。

两段历史可能留下相同的持久文件：①受控新版本只读探测；②同一目录保留 marker，旧版不认识它，曾进行未被新日志记录的物理动作后退出/崩溃。仅检查空锁、mtime、缺少 backup、下载文件 SHA、当前 disconnected/PID 不存在，不能区分这两段历史。marker 签名或增加 schema 也不能单独证明旧程序没有运行。556 的实际同目录用例是**旧版先运行、无 marker，再升级隔离**，没有验证此逆向情形。

## 建议实施顺序与验收

1. **先收紧证明模型，再允许自动模块 admission。** 用纯状态 `CurrentBootClean / VerifiedRecovery / UnknownLegacy` 区分当前启动建立的证明、实际恢复回执与未知历史；持久化 v1 marker 本身不能授予跨旧版本的可信 lineage。无法证明时只隔离模块，listener、reader/PCSC/BLE、N 待和 reader VoWiFi 保持。所有 accepted/unknown 本次物理工作继续原清理栅栏；不通过全局启动失败或清理后猜测绕过。
2. **为支持的回退提供显式 launcher。** 在 spawn 旧版前持久化并 fsync 独立的 `legacy_execution_pending` 记录（schema/run-id/旧 binary SHA/阶段），保留原 marker 与恢复文件；新启动先看 pending，优先于 pristine。旧进程非零退出、launcher 崩溃、缺少回执均保持 unknown。旧版退出 0 也不能单独清除历史恢复状态。该机制仅覆盖受控 launcher，不能追认已经发生的未包装回退。
3. **未包装回退必须 fail closed 或明确保留 blocker。** 若验收要求覆盖任意旧版直接运行，则需把来源不明的持久 marker 归为模块 UnknownLegacy；不能同时宣称仅凭现有文件就保留无感模块重启。此策略会影响正常只读锁目录的模块重启，需要主审明确产品行为并配套显式恢复路径。恢复应核对真实模块身份、已验证备份/配置和 route/owner 关闭回执；缺少原始证据时保持未知，不自动删除文件或写“已恢复”。本轮不设计或执行物理 reconcile。
4. **旧 exit 1 只能修旧实现，不能修评分。** 找回匹配 3.42 源码后，对新的、独立标识/SHA 的兼容回退构建前置 backport 模块隔离与 owner admission：只有启动前已阻止所有模块物理入口、且本进程无 accepted/unknown 工作、所有自有 owner/child/socket 确认排空时可退出 0；历史恢复仍 false。必须审阅旧 Rust/Go 全部入口，不能在收到 ADB 清理错误后改判。未修改的归档 3.42 继续原样测试并记录 exit 1；找不到精确源码时此项保持失败。

先做离线纯函数/生命周期测试：有效 marker 后模拟 unknown legacy history 必须隔离；正常受控只读历史由明确 authority 放行而非 marker 单独放行；pending 写入/spawn/退出/fsync 各 crash point 均不得变 clean；旧 exit 1 不能清 pending；任意本次 accepted/unknown 工作不能省略 finalizer；读卡器与通用 SMS 的 reader 分支不受影响。然后在原 546 mandatory sandbox、复制 bundle/合成根中重跑无 marker 升级、受控回退、残留 marker 负例，保存原退出码与证据指纹。纯文件模拟不证明旧版真实物理动作或设备恢复，硬件验收仍需另行授权。

本轮仅新增本文；没有修改生产代码、inventory、原始基线或测试判分，没有运行 daemon、硬件/代理/外连或生成 DMG。
