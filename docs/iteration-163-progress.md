# Iteration 163：转发账本空状态恢复与跨进程未知结果防重放

日期：2026-10-08。改动只在 Switch 中完成。测试使用本机合成 Bark 接收端和合成短信正文；没有调用真实Telegram/Bark服务、读取用户短信/密钥、连接读卡器或SIM、访问运营商网络、构建App或DMG。

发现设备短信转发首次启用时，准入账本处于空状态，通用archive `mutate`仍会把无变化的空数组写成`[]`。`GuardedArchive`会把“存在但为空”的账本视为损坏，因此完整Rust进程重启后重新配置返回 `device_forwarding_admission_unavailable`。现改用有变更才落盘的事务：无handoff的首次启用不产生空文件；实际启用状态变化仍保持原有耐久写入。

为兼容已经落盘的旧`[]`空账本，`ArchiveStore::remove_if_empty`在同一跨进程锁内读取、验证并删除确实为空的文件，随后同步父目录；非空数据保留，畸形/不安全文件仍失败关闭。该迁移只用于设备短信准入账本，其他archive语义不变。

Rust/Go进程集成新增本机合成provider场景：将合成Bark请求完整交给loopback服务器并保持确认未返回，确认队列已经持久标记为`submitting`后强杀Rust进程；重启同一数据目录并重新配置后，任务恢复为`paused_result_unknown`、attempt保持1。观察完整5秒worker轮询周期，外部请求仍只有1次；显式重试才发出第二次请求并成功完成。测试同时覆盖首次配置、旧空账本归一化和真实Rust daemon进程退出/重启，不触碰外部网络。

验证通过：

- `cargo +1.98.0 test --manifest-path rust/Cargo.toml -p proletarius-store`：8项通过。
- `cargo +1.98.0 test --manifest-path rust/Cargo.toml -p proletariusd`：951项通过、3项忽略。
- `python3 scripts/test-multistandby-integration.py`：19项Rust/Go隔离进程检查通过，含新增短信转发强杀/恢复/显式重试流程。
- `python3 -m py_compile scripts/test-multistandby-integration.py`、Rustfmt及后续`git diff --check`通过。

此修复增加P2A **+0.025计划人周等值**：由3.835升至**3.860/5.5（70.18%）**。总进度由13.670升至**13.695/47.5计划人周（约28.8%，估算误差约±3个百分点）**。此次进度只计空账本跨版本恢复与provider结果未知的进程级耐久证据，不代表真实provider投递、系统PAC、完整Go退出或实机短信验收完成。继续未生成DMG。
