# Iteration 201：模块 eSIM AT/APDU 适配底座

日期：2026-10-09。范围限定在 `/Users/proletarius/Desktop/Switch`。

新增 Rust `ModuleEuiccChannel`，把模块侧 `AT+CCHO`、`AT+CGLA`、`AT+CCHC` 转换为已有 Rust APDU transport 接口，可供 `proletarius-euicc` 执行 ES10c Profile 操作复用。每个 APDU 使用固定超时和响应上限；logical-channel 编号、AID、十六进制响应均做边界检查；AT 返回错误、畸形响应及传输不确定时不自动重试。关闭会先将本地 channel 标为不可复用，避免关闭结果不确定后误用旧通道。

本轮是协议适配层准备，不代表模块 eSIM API 或生产 USB AT owner 已接入，也不计入迁移完成率。当前仍以 Iteration 200 看板为准：**14.770/47.5计划人周，31.1%**。下一步需把该适配器接入 Rust USB AT 完整事务准入与模块射频 `eSIM` reservation，再迁移模块 Profile 列表/切换业务并与旧 API fixture 差分；在完成生产接线前，不据组件测试提高总体进度。

验证：`cargo test -p proletariusd module_esim_channel` **4项通过**，覆盖 CCHO/CGLA/CCHC 序列、APDU 转换、错误不重试、关闭单次语义和输入/响应边界；daemon 全量测试 **1,101 passed / 3 ignored**，Rustfmt 与 `git diff --check` 通过。未接入或操作实体读卡器/SIM，未切换/下载 Profile，未发送短信、拨号或生成 DMG。
