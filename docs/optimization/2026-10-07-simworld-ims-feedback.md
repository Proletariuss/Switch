# SIMworld SMS 与 Surge 路由复核

日期：2026-10-07。只记录状态与建议；未读取、复制或保存验证码正文。

## 本次短信验证

- 重启后，SIMworld IMS 于 19:45:57 完成注册。19:46:06 的新入站短信通过 TPDU 解码；本地持久化回调没有报错，随后发送 RP-ACK。
- 19:46:09 的 RP-ACK SIP MESSAGE 被网络以 403 拒绝。此前新增的 delivery-report 构造仍未解决回执问题；短信接收路径已触发，但不能据此保证短信中心确认送达。保持为未解决兼容缺口，不宣称修复完成。
- 读取验证只用了事件时间、结果与状态，不读取短信正文。不要在诊断日志中输出短信正文、号码或原始 TPDU。
- 用户指定的语音编码偏好仍为 EVS 优先、仅在对端不支持 EVS 时回退 AMR。当前运行记录显示测试呼叫使用 AMR；Switch 尚未集成 EVS 编解码/协商，故这项要求还未实现，也未在本次测试中验收。

## Surge 当前状态与建议

只读检查到活动规则 `DOMAIN-SUFFIX,wlan.three.com.hk,🇭🇰 VoWiFi-HK`。`VoWiFi-HK` 已经是自动测速组，且成员仅为香港 13、14、15 家宽；当前选中 13 家宽。19:52 的测试读数分别约为 34、37、39 ms；较早一轮（19:45）读数约为 37、34、30 ms，排名会随测试变化。

组参数为间隔 300 秒、超时 5 秒、容差 100 ms，启用了“使用前评估”。100 ms 容差允许保留一个并非当前最低延迟的成员，因此它不保证严格选择最低读数。建议把容差改为 0 ms，其余参数与三个家宽成员暂时保持不变。零容差可能因小幅测量抖动更频繁换线；组切换可能中断正在运行的 ePDG/IKE 隧道。

Surge 自动测速依据测试 URL 的 HTTP HEAD 响应时间，并不等同于到 SIMworld ePDG 的 UDP 500/4500 可达性、丢包率或 IP 信誉。应分别记录选中代理、HTTP 延迟、ePDG/IKE 建隧道结果和隧道重连，不能单凭 HTTP 延迟宣布节点适合 VoWiFi。参考：[Surge 自动测速策略组文档](https://manual.nssurge.com/policy-groups/url-test.html)。

本次仅检查并关闭了策略组编辑向导，没有保存改动；活动规则与组配置未修改。Surge 属于网络设置，按界面操作策略，需要在保存 `tolerance=0` 前取得该次操作确认。当前已形成的变更项是：仅对 `🇭🇰 VoWiFi-HK` 将容差从 100 ms 改为 0 ms，不更改其它策略、全局代理或规则。

## 运行状态

本次为加载短信回执修复重启过 Switch。重启时 IMS 曾重新注册；监控过程中进程退出。随后重新打开的 2.83 应用连接到同一 `build/rt-simworld` 数据目录，UI 显示读卡器已连接。再次开启 VoWiFi 后，UI 显示 IMS 已注册，但 SMS 通道尚未就绪。需在稳定运行的实例上继续复测回执；当前不能把 403 结论标为已解决。

## 后续验证码复核

20:02 再次只检查收件状态后，短信中心仍只有 1 段对话、1 条较早的入站短信；没有新增验证码记录。Reader 页面仍显示“IMS 已注册，SMS 通道尚未就绪”。运行日志最新的两次 readiness 结果均为 `route_metadata=true`、`transport=false`；19:46 之后没有新的入站短信解码事件。因此本次验证码未进入 Switch 的接收与归档路径，尚不能归因于短信列表刷新或展示。

同时核实到安装状态：`/Applications/Switch.app` 和当前 `DerivedDataCallFix/Build/Products/Debug/Switch.app` 都标记为 2.83（283），但主程序与 `switchd-rs` 的 SHA-256 不同；已安装主程序时间为 18:16，Debug 主程序时间为 19:42。现有 `Switch-2.83-macOS27.dmg` 时间为 18:16，早于该 Debug 构建。故这次现场测试运行的是旧安装包，不是更新后的 Debug 产物；尚未生成新版 DMG，也未验证新构建的实机短信表现。

20:05 已从 Switch 目录直接启动 `DerivedDataCallFix/Build/Products/Debug/Switch.app`，通过进程路径确认没有误启动 `/Applications/Switch.app`。读卡器重新连接后，在 Debug 构建中启用 SIMworld VoWiFi；UI 显示“Wi-Fi Calling 已就绪，SMS 与通话可用”以及“VoWiFi 已连接，SMS 就绪”。收件箱仍为 1 段对话；由于用户此前发送验证码时旧应用的 SMS 传输尚未就绪，这条短信未能验证最新 Debug 构建的接收能力。需要在当前就绪会话下再触发一次新入站短信，才能验收接收、归档与 RP-ACK；本次未生成 DMG。

20:14 用户从 Bilibili 再次触发验证码后，当前 Debug 实例已收到并归档新的入站短信；只核对记录时间、方向和归档条数，没有读取或记录短信正文/验证码。最新记录时间为 20:14:54 +08:00，收件箱归档共 2 条。此结果确认本次入站解码与本地持久化成功。对应 RP-ACK 仍收到 SIP 403（20:15 左右），因此网络侧交付确认仍是未解决项；本次不做外发短信测试。

用户随后明确要求生成 DMG。发布版本升为 2.84/build 284；Rust 锁文件只更新了 21 个本地 workspace 包版本，第三方依赖锁定保持不变。全 Rust workspace、Go vet/完整测试及跨语言 parity gates 通过。Release 构建和镜像验收结果见本轮交付记录。

DMG：`/Users/proletarius/Desktop/Switch/dist/Switch-2.84-macOS27.dmg`，50,014,086 字节，SHA-256 `ae54d197af5dedcc38ccd07ae8e6e4195535edf88c933f1e8c4d5fecfbbbf221`。镜像校验、内嵌 App 严格签名校验及 App/Rust/Go 二进制与 Release 产物逐字节核对均通过。Apple Silicon arm64，最低 macOS 27.0；使用本机 ad-hoc 签名，未做 Apple 公证。完整校验记录：[2026-10-07-switch-2.84-dmg-verification.txt](../../build/optimization/2026-10-07-switch-2.84-dmg-verification.txt)。
