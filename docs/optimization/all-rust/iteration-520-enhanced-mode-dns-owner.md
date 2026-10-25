# Iteration 520: 仅 Surge Enhanced Mode 的 DNS 所有权

2026-10-18。上一轮已修复被 macOS 系统代理识别的 Surge 路径；本轮补上“Surge Enhanced Mode 开启、系统代理未开启”的传输选择边界。所有改动只在当前 Switch_MAC 源码中；没有修改 Surge 开关、实体卡、已安装 App，也没有生成 DMG。

当系统代理被明确识别为未配置时，Rust 现在查询 Surge CLI 的 `enhanced-mode` 状态。确认 `On` 就将 ePDG 解析交给 Surge DNS，并且只接受已验证的 `utun` 路由；确认 `Off` 或未安装 Surge CLI 时，保留原有无前置代理的直连选择。CLI 已安装但状态无法读取时，选择失败并报告固定的 `surge_enhanced_mode_unavailable`，不会把“不知道”当成“已关闭”而发起主机 DNS 或直连 UDP。已配置的系统/显式代理仍优先走其已验证的 SOCKS5 UDP 或 Surge 透明路径。

单测覆盖 Enhanced Mode 的开启/关闭解析、`utun` 与非 `utun` 路由、状态未知时的提前拒绝。当前机器的 `surge-cli feature get enhanced-mode` 回报 `On`，但本轮没有切换 macOS 系统代理开关，因此“仅 Enhanced Mode”组合属于离线行为验证，并非该组合的实机验收。SIM World 的 IMS/ESP 故障仍按 [Iteration 519](iteration-519-surge-dns-simworld.md) 记录处理。

完整 Rust workspace 回归为 3034 通过、0 失败、1 忽略，Rust 守护进程构建通过；迁移契约门禁为 294/294 Rust HTTP owner，127 个 Swift 调用点和 430 个复核范围。格式与 diff 检查通过。日志见 `build/reports/iteration-520-rust-workspace.log` 和 `build/reports/iteration-520-rust-build.log`；当前 HTTP owner 归属不因 DNS 选择修正而增加迁移分。下一步需要对实际 ESP 出站和回程取得更强证据，同时继续 P0–P6 其余业务及长期验收。
