# Iteration 368：IMS Digest challenge 选择迁入 Rust

日期：2026-10-11

读卡器 IMS 注册和附属 `voiceclient` 的生产路径现在把 Digest challenge 的来源选择、候选拆分、解析和优先级评分交给 Rust IMS codec owner。Rust 保留两处已有差异：`imscore` 使用首个可解析 challenge，`voiceclient` 按 realm、AKAv1/AKAv2/MD5 算法和 nonce 解码长度选择最高分候选。多挑战扫描在引号外识别逗号边界，不会把引号参数中的 `Digest ` 文本误拆成新 challenge。`WWW-Authenticate` 优先及 407 时仅在其缺失后回退 `Proxy-Authenticate` 的来源顺序保持不变。

主 reader session 将 Rust selector 注入 `imscore` 与 `voiceclient`；生产回调错误或响应不合规时在物理 AKA 前失败关闭。未配置 selector 的旧兼容调用保留原 Go parser/selector。Rust 的输入 header 数量、长度、realm、候选输出字段均有界。物理 AKA、SIP stream/socket、事务等待和 IMS 生命周期仍由 Go 承担，因此该切片不代表单 Profile VoWiFi 闭环已迁移。

新增 Rust daemon 测试覆盖 first/best 两种策略、AKAv1 与 realm 加权、407 challenge 来源以及引号内逗号；新增 Go bridge、`imscore` 和 `voiceclient` callback 测试。完整 `scripts/test-migration-gates.sh` 通过：Rust workspace **1,910 passed / 6 ignored**（daemon **1,373 passed / 3 ignored**），Go 全包 test/vet、Swift BLE 回归和现有协议 parity 通过；`cargo fmt --check`、契约 `--diff`/工具自测及 `git diff --check` 通过。门禁日志：[iteration-368-migration-gates.log](../build/optimization/iteration-368-migration-gates.log)。未连接实卡、未访问运营商网络、未发送短信/拨号，也未生成 DMG。

P3 增计 **0.025 计划人周等值**，累计 **1.940 / 8.0**；项目累计 **16.420 / 47.5 = 34.5684%**，显示 **34.6%**（两位小数 **34.57%**，估值误差约 ±3 个百分点）。
