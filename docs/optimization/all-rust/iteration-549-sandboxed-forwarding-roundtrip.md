# Iteration 549：SWF1 加密转发队列反向写入往返

2026-10-18。审计 Iteration 546 与源码契约门禁后，补齐一项明确的离线缺口：
294 条接口的 Rust 所有权与源码审阅不能证明持久化加密队列的旧/新二进制
写后兼容；Iteration 513 的 SWF1 测试只覆盖当前 Rust 读取 Go 生成密文、
取消和当前 Rust 重启，没有实际旧版再次写入。Iteration 546 的四文件验证
及原报告保持不变。

## 本轮变更

- 新增独立 opt-in 脚本 `scripts/check-p6-forwarding-roundtrip.py`，复用
  Iteration 546 的强制沙箱、启动与进程组退出检查，不修改该辅助脚本。
- 输入仅为已提交的 `go-state.swf1.hex` 合成 golden，密钥是该 fixture 公布的
  `00..1f` 字节；阶段间仅复制 `sms-forwarding/state.enc`，不复制配置、锁或
  其他运行数据，不读取 Keychain、真实用户数据或实体卡。
- 将五项无硬件沙箱正/负例测试接入 `scripts/test-migration-gates.sh`。完整
  门禁只运行这些沙箱边界测试；实际旧版往返仍需显式提供二进制，未成为
  默认门禁对旧版包的隐藏依赖。

## 实际验证

| 阶段 | 动作 | Telegram / Bark 结果 | 退出 |
| --- | --- | --- | --- |
| 旧 3.42 gateway + Go | 读取 Go golden | paused_disabled / paused_disabled | 1，既有退出缺陷 |
| 当前 Rust | 读取后取消 Telegram | cancelled / paused_disabled | 0 |
| 旧 3.42 gateway + Go | 读取新版密文后取消 Bark | cancelled / cancelled | 1，同一既有缺陷 |
| 当前 Rust | 读取旧版再次写出的密文 | cancelled / cancelled | 0 |

四阶段均先提交错误测试密钥，断言 HTTP 400 与
`sms_forwarding_queue_unavailable`，并以 SHA-256 验证原密文不变；随后用正确
测试密钥打开队列。两个 provider 从配置到状态始终禁用，未提供 provider
凭据或 URL。所有阶段均在新建的 `build/runtime/p549-*` 目录运行。

每次阶段前后检查都保留 Telegram 的 2 次历史尝试、Bark 的 1 次历史尝试、
Telegram 已确认 1/总计 2 分片进度及未知提交错误码。未绑定的旧格式任务
保持暂停且尝试为 0；两渠道取消后目标任务只出现一次，并转入终态诊断，
pending 数从 2 降至 1，再升级读取不复活。原始回退密文哈希全程未变。
报告记录可执行文件、golden、脚本、辅助脚本、沙箱策略及各阶段输入/输出
密文的 SHA-256，并检查可执行文件在运行期间没有变化。

每阶段启动前，父进程与继承子进程的沙箱探针全部通过。沿用 546 的内核
约束：拒绝 IOKit 打开、Mach 服务查找、设备文件内容访问、阶段外写入和
阶段外出站连接，仅允许 `/dev` 目录枚举/路径元数据及 null/随机数例外。
没有打开 USB、PC/SC、BLE 或物理串口，没有接触或停止已安装 daemon。
所有测试自有进程组均退出。旧版仍发生已知的通话模式退出错误；脚本只将
已知退出 1 单列为限制，其他非零退出和残留自有进程均导致验证失败。

五项沙箱边界测试通过；源码契约门禁复核通过，保持 294/294 Rust 接口、
127 个 Swift 调用点、430 个审阅范围及 54 条 HEAD 别名；Go 退出结构门禁
通过。没有修改生产代码、固定 golden、接口审阅指纹或运营商
文件，没有生成 DMG。

报告：`build/reports/iteration-549-forwarding-roundtrip.json`，相对
`Switch_MAC` 根目录；`passed=true` 只代表本脚本限定的数据往返，
`full_p6_acceptance=false` 保留完整验收边界。

## 复现与剩余边界

从 `src` 执行；当前 Rust 二进制可用 546 文档的离线构建命令生成：

```sh
python3 -m unittest scripts/tests/test_p6_data_roundtrip.py -v
python3 scripts/check-p6-forwarding-roundtrip.py \
  --current ../build/rust-target/debug/switchd-rs \
  --legacy-backend ../build/release-3.42/Build/Products/Release/Switch.app/Contents/Resources/backend \
  --report ../build/reports/iteration-549-forwarding-roundtrip.json
```

本轮仅证明固定合成 SWF1 的取消、分片确认和尝试状态在实际旧/新后端间
副本读写兼容。未测试真实 Keychain 升级、密钥轮换、真实 provider 投递、
自动重试/恢复投递、其他加密格式、同目录原位回退、旧 UI、安装及全应用
性能；旧版完整退出仍未通过。没有据此完成 P6 或提高整体完成百分比。
