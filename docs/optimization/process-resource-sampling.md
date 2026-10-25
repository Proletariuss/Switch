> 历史记录：以下内容来自 Proletarius，包含当时的功能、路径、发布要求与验证结果，仅供追溯，不是 Switch 的当前指令或验收结论。Switch 已移除传真，当前构建与执行范围以根 README 和桌面工作区改造方案为准。

# 显式 PID 的只读资源采样

`scripts/sample-process-resources.rb` 只向 macOS `ps` 查询指定 PID 的状态、开始时间、可执行路径、累计 CPU 时间及 RSS。它不会按名称自动找进程，不读参数、环境、token、socket、日志或用户业务数据，也不会启动、停止、重启 App 或修改代理。需要 Ruby 2.6+ 和 macOS；不依赖第三方 gem。

先确认本轮 Swift、Rust、Go 的三个 PID，再创建私有输出目录。下面的 PID 仅是本轮示例，进程重启后必须重新确认并使用新文件：

```sh
umask 077
SAMPLE_DIR=$(mktemp -d /tmp/proletarius-soak.XXXXXX)
ruby scripts/sample-process-resources.rb \
  --pids 68827,68831,68832 \
  --output "$SAMPLE_DIR/resources.jsonl" \
  --interval 30 --duration 86400
```

脚本不会自动长跑，只有显式执行才开始。输出文件以排他方式新建、权限 0600，每条 JSONL 都 flush/fsync；已有文件、符号链接、非私有文件和并发写入均拒绝。父目录需事先存在。原始记录留在本机，不应直接发布带本机路径的文件。

每次采样保存 UTC、纳秒单调时钟、累计 CPU 秒、RSS KiB、存活状态和间隔 CPU 百分比。后者按 `100 × Δ累计 CPU / Δ单调时间` 计算，多核进程可能超过 100%。首次采样没有 CPU 差分；三进程 RSS 相加可能重复计算共享页，不是唯一物理内存或耗电。`ps` 开始时间分辨率为一秒；身份同时核对开始时间和可执行路径，CPU 计数倒退也记为断档。

进程消失、僵尸、身份改变、CPU 倒退、采样延迟及睡眠/墙钟异常均留下断档，后续进程恢复也不会抹去。查询失败标为 `unobserved`，不误称进程退出。断档窗口不计算 CPU 合计；缺失或身份错误的进程不计入 RSS 合计。固定 PID 不会被新进程自动替换。采样边界间的短暂事件可能不可见。

Ctrl-C 或只向采样器发送 SIGTERM 会写入 `session_end` 并退出，目标 App 不受影响。续写使用同一组 PID，并明确给出这一段的持续时间：

```sh
ruby scripts/sample-process-resources.rb \
  --pids 68827,68831,68832 \
  --output "$SAMPLE_DIR/resources.jsonl" \
  --interval 30 --duration 3600 --resume
```

续写保留原始数据和原始进程身份，必定记录观察中断，不拼成连续 24 小时。若文件末尾被强制终止截断，工具拒绝追加；保留旧文件并选择新文件，不自动删除或修复历史。

退出码：0 表示该次时长已到且观察期间无已知断档；2 表示采样时长已到但存在断档（包括续写）；1 表示输入/工具错误；130/143 表示采样器收到 INT/TERM。`duration_complete` 与 `continuous_so_far` 分别表示时长和数据连续性。任何退出码、CPU/RSS 或进程存活都不能证明 IMS 在线、通话可达、BLE 稳定或 24 小时业务验收通过，业务状态必须独立验证。

运行验证：

```sh
ruby scripts/test-process-resources.rb
```

测试覆盖 macOS `ps` 解析、CPU 差分、进程丢失/恢复、PID 身份变化、僵尸、时钟/采样断档、私有续写、并发/链接拒绝，以及采样器 SIGTERM 正常收尾。真实短采样只观察测试 Ruby 自身，不启动或接触 Proletarius。
