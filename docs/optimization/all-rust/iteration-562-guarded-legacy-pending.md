# Iteration 562：受控旧版 pending 的离线协议验收

2026-10-18。本轮新增持久 pending 证据、模块启动 Gate 的只读拦截，以及强制 sandbox 的合成验收工具。生产回退的并发互斥与静态路径信任尚未闭合；本轮不构成生产回退支持，也不表示 P6 完成。

## 实现与耐久性

`scripts/check-p6-guarded-legacy-launch.py` 是离线验收工具。CLI 必须显式提供 `--data-dir`、`--binary`，只在 Switch `build/runtime` 下创建不存在的新合成根目录，不接收已有业务目录或默认用户目录。新根含固定测试 seed；数据目录、binary 的路径遍历拒绝符号链接。该限制服务于离线验收，不是对恶意并发路径替换的生产安全证明。

固定证据文件为 `.legacy-execution-pending-v1.json`。记录包含 `schema=legacy_execution_pending_v1`、随机 `run_id`、目标 binary 的 SHA-256，以及 `phase=spawn_pending`。不记录业务身份、密钥、环境或目标命令参数。

创建流程为：

1. `mkdir` 新合成数据根后，先 `fsync` 其父目录，再写测试 seed。父目录同步失败即拒绝后续初始化与目标启动。
2. pending 以 `O_CREAT | O_EXCL | O_NOFOLLOW`、0600 创建；已存在、无效、不可读、目录、软链接、硬链接等证据均不覆盖。
3. 循环完成全部写入，依次 `fsync` pending 文件、数据根目录，两道屏障通过后才允许启动目标 binary。写入或同步失败保留已产生的残缺证据。

文件同步与数据根同步不能替代新数据根在父目录中的目录项同步；第 1 步是独立审查指出后补入的修复，已有顺序和失败注入测试。

pending 永不因目标退出、launcher 异常或崩溃自动删除；目标 exit 0 也不构成物理恢复回执。SIGTERM/SIGINT 转发到本次创建的独立 process group，仅清理该组，宽限期后仍未退出则强杀并保留非成功退出。目标自行返回 exit 1 时仍返回 1；因信号死亡按 `128 + signal` 表示。观察到组已不存在后，仅进行有界 `child.wait`，不再追加 SIGKILL；wait 超时仍视为 termination unconfirmed。

`module_recovery.rs` 在 pristine 判定前检查固定 pending 路径。路径存在，无论内容与形态，均加入 `LegacyExecutionPending`；元数据查询异常加入 `UnreadableEvidence`。检查不跟随链接、不改写证据，也不授予 `historical_recovery_confirmed`。现有模块与读卡器分支归属不变。

## mandatory sandbox 与实际验收边界

工具复用 `check-p6-data-roundtrip.py` 的 546 sandbox policy、`validate_sandbox` 与合成环境。目标启动前运行 kernel canary，包括继承到子进程的验证；策略验证失败、sandbox-exec 不可用或非 macOS 时均无不受限启动的 fallback。

复用策略拒绝 IOKit、Mach lookup、设备文件访问、合成根外写入与外部网络连接，只保留合成根内 Unix socket 等本地验收通道。canary 使用策略查询、`/dev/zero` 拒绝检查及合成 socket，不打开真实读卡器或模块。目标环境的 `HOME`、`TMPDIR`、`SWITCH_DATA_DIR` 均绑定合成根，BLE 指向合成不存在路径。

本轮实际执行的是合成 Python dummy executable，没有运行归档旧版、连接运营商、发送短信、操作 SIM、修改真实业务数据或生成 DMG。真实归档旧版只能在主审另行协调的同等隔离验收中使用；本报告不把 dummy 的退出与清理结果外推为旧版真实设备恢复。

macOS 无法以本轮尝试的 `/dev/fd/N` 执行方式绑定已哈希的可执行句柄；离线工具使用静态可信 fixture 路径，并在启动前核对文件元数据。该检查不消除所有路径或文件内容并发替换窗口，不能作为生产 atomic binary identity 证明。

## 已验证

执行 `python3 -m unittest scripts/tests/test_guarded_legacy_launch.py -v`：最新 **17/17 通过**。此前完成父目录 fsync 修复时为 16/16；最后新增 observed-gone group 的有界 wait、禁止追加 SIGKILL 及 wait timeout 负例后为 17 项。

覆盖显式参数、fresh-root 限制、父目录同步顺序及失败、旧证据各形态不覆盖、部分写入、文件和目录同步失败禁止启动、记录持久后 launcher 崩溃、exit 0/1、信号转发后保留 exit 1、有界强杀、自有后代清理，以及无关进程仍存活。真实 dummy 用例均通过 mandatory sandbox；耐久性顺序与失败路径采用注入测试。

主审使用共享 Rust target 完成的定向结果，已核对日志：

- `build/reports/iteration-562-preflight.log`：16 passed，0 failed。
- `build/reports/iteration-562-quarantine.log`：28 passed，0 failed。

上述日志位于 Switch 根的 `build/reports`。本工具实施任务没有另起 Cargo 构建，没有修改 contract inventory、归档旧版二进制或退出评分。

主审随后使用563完整签名包内 daemon 运行 pending+pristine 合成启动验证，报告 `build/reports/iteration-562-pending-bundled.json` 为 passed：reason包含 legacy_execution_pending、历史恢复false、模块操作409、reader和合成数据投影保留、pending字节/元数据不变、退出0/自有socket清理，临时root删除。受测binary和源码边界见[563完整构建记录](iteration-563-amr-wb-cmr.md)。这仍是mandatory sandbox中的程序行为证明，不是生产并发或物理恢复证明。

## 仍未解决

Gate 仍为启动快照。尚无覆盖所有相关 daemon 与 launcher 的共享 data-dir lifetime lease，因此本工具不能证明与已经运行的新版本并发时的生产回退安全；未包装旧版直接运行也不在覆盖内。静态目录和 binary 信任、真实物理恢复回执及旧 3.42 的原 exit 1 问题继续保留为 P6 blocker。不得因本轮 pending 协议通过纯测试而自动清除历史恢复状态或宣称模块已恢复。
