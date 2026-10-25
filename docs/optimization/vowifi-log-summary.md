> 历史记录：以下内容来自 Proletarius，包含当时的功能、路径、发布要求与验证结果，仅供追溯，不是 Switch 的当前指令或验收结论。Switch 已移除传真，当前构建与执行范围以根 README 和桌面工作区改造方案为准。

# VoWiFi 离线脱敏日志汇总

在仓库根目录运行；仅读取指定日志并向标准输出写 JSON，不请求 API、不读取 App token、不改变正在运行的 App、设备或代理：

```sh
ruby scripts/summarize-vowifi-log.rb \
  --log '/Users/proletarius/Library/Application Support/Proletarius/proletarius.log' \
  --start '2026-09-26T15:10:07+08:00' \
  --end '2026-09-26T15:21:44+08:00' \
  --log-offset '+08:00'

ruby scripts/test-vowifi-log-summary.rb
```

示例结束时间是固定的历史分界；每次采样应显式替换。起止时间必须包含时区，窗口为 `[start, end)`；`--log-offset` 只解释日志中没有时区的时间戳。结果时间统一输出 UTC。可用 `--gap-seconds 90` 调整证据空白的报告阈值；它不是生产超时配置。

输出采用固定字段白名单，不包含日志原文、源文件路径、trace_id、号码、IMSI、IP 或密钥。trace_id 仅在内存关联，`session` 是本次窗口内的顺序编号，不能跨报告当作稳定身份使用。每种事件输出次数及首次、末次时间；REGISTER 和 OPTIONS 成功都要求标记中的状态码是数值 2xx。

## 如何解释

- `observations_only` 仅表示观察到了有 trace 的成功标记；没有 `pass` 结论。空日志、窗口内无事件、仅无 trace 成功等情况为 `insufficient_evidence`。
- `options_missed`、`options_recovered`、`options_failed` 只统计明确日志标记，不根据应答间隔或 `sent - acknowledged` 推算丢包。恢复标记是日志观察，不是脚本补造的事务关联。计数未去重，重复日志可能重复计数。
- `lease_scheduled` 是续期计划，`lease_renewal_due` 是计划触发，都不是续期成功。新 REGISTER 是另一条成功观察。传输停止标记不自动判定为异常故障或正常退出。
- 缺失、空或非字符串 trace 的有效事件进入 `unattributed`；不按时间邻近猜测会话。生产计划续期及停止日志通常没有 trace。坏 JSON／候选消息格式不匹配单独计入 `event_format_unmatched_lines`，不归类为缺 trace。
- 每个 session 的 `unknown_gaps` 都按**整个请求窗口**计算，包括该 session 出现前和最后观察之后。因此计划更换 session 后，旧 session 的尾部、新 session 的头部都可能产生空白；这不是该 session 存续期间已证实的断流。无成功观察时整个窗口为未知。90 秒内没有空白报告也不证明连续可用。
- `unscoped_timestamp_unmatched_lines`／`unscoped_candidate_lines` 无法确定时间归属，统计范围是整个输入文件；其余窗口诊断已按时间过滤。格式未匹配、日志轮转、缺失、重复、休眠、进程停止和时钟变化都限制完整性判断。
- OPTIONS 最终 2xx 证明对应 SIP 成功应答，不证明短信送达、真实来电／媒体成功或 BLE/APDU 持续可用。某窗口 `dpd_disabled=0` 仅表示没观察到该标记，不表示主动 DPD 已启用。

当前有效期 2400 秒的注册，生产策略通常在 35 分钟后重建会话续期；应以实际 `lease_scheduled` 记录为准。24h 报告必须跨越实际采样时长，并区分计划续期与异常恢复，不能把短时样本外推为 24h 通过。

fixture 使用临时合成日志，覆盖时区与半开窗口、生产设备消息前缀、ANSI 控制台、最终响应、会话隔离、缺 trace、坏格式、未知空白、空输入以及 stdout/stderr 脱敏；不接触真实设备或生产状态。
