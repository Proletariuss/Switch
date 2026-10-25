# 3 HK / SIMworld 用户模板兼容记录

日期：2026-10-07。目标版本：Switch 2.82 / build 282。用户要求按其 `3_hk.json` 调整 VoWiFi，并在完成后输出 DMG。

## 配置与范围

用户提供的原始模板 SHA-256 为 `e230b9b819f319b6ab0c88878879bdac78c1653bc733e80215c98a7738196e84`。本轮仅为修复 IKE 源端口互操作，在 `ike` 中加入 `local_port: 500`；当前嵌入模板 SHA-256 为 `240333084338362587eb5c495a0fcd749c315cd4f1a1a6e4c58442b3a5054e94`。Rust 运营商运行时响应与 Go 兼容执行层共享这一输入，不改写原 Apple registry/YAML。

覆盖条件为已选择通用 `apple_hutchison_hk` 且归属 PLMN 为 454/03。DENT、HKBN 等已命中专有选择器的虚拟运营商，以及 454/04 和其它国家/运营商保留原配置。实卡 UI 确认本次 SIMworld 测试卡为 454/03；结论只针对该 PLMN。

原有 YAML 显式覆盖以及应用内自定义 ePDG 优先级保留；模板在新建会话时生效，不改动已运行会话。

## 来源核对

用户确认配置来自 [voorz/vohive-release](https://github.com/voorz/vohive-release)。发布文件只作只读格式检查，未执行安装脚本或二进制。模块元数据指向公开依赖 [voorz/vowifi-core v1.4.5](https://github.com/voorz/vowifi-core/tree/eddbf72e6f722fdd2b10a911ee5212d158dce784) 和 SWu v0.3.8；以该版本的身份、注册和报文行为解释字段，避免根据名称猜测。

只参考协议行为，使用 Switch 现有库独立实现；不复制未确定许可的实现，也不把发布镜像的许可当成业务实现许可。上游额外字段不自动带入，尤其不关闭 EAP MAC 校验。研究材料存放在 `Switch/build/optimization/2026-10-07-three-hk-reference/`，不进入源码或安装包。

用户补充确认，`Downloads/3_hk.json` 是 VoHive 方案实际使用的 3 HK 配置模板。该文件哈希与 Switch 嵌入副本的原始哈希一致；其核心字段也与固定版本 `voorz/vowifi-core` 的 `runtimehost/carrier/profiles/3_hk.json` 相符：两者都指定 `identity_source=isim`、`initial_authorization=eap_direct`、`wlan.three.com.hk` 和同一组 3HK IKE/ESP 提案。固定版本还包含用户文件未提供的 `eap_mac_validation=false`、`include_pani=false`、`include_pani_authenticated=false` 等字段；这些额外设置没有自动并入 Switch。

VoCat 的 `ipcc-hutchison-hk-45403` 规则匹配 PLMN 45403/45404 或 ICCID 前缀 8985203，选择 `wlan.three.com.hk`、modern IKE、可选 IMS IPsec 和自动 PANI。该 3HK 规则没有声明专用 IMS 身份或 REGISTER 模板；VoCat 的通用 IMS 路径使用 IMSI/归属 PLMN 派生身份，完整 ISIM 存在时才改用 ISIM。此身份选择与 Switch 的实现相符。VoCat 面向 Quectel/ML307 模组并依赖 Linux XFRM，不能据此推断它已验证 SIMworld eSIM 或其 IMS/VoWiFi 授权。

因此两个项目为 3HK 网络参数、身份选择和 REGISTER 兼容策略提供了有价值的参考，但没有给出能绕过网络 IMS 拒绝的额外凭据。实卡已完成 EAP-AKA 与 IKE/ESP；随后所有模板变体均收到 P-CSCF 的 403。本轮未发现尚未应用且有证据能改变该拒绝结果的上游参数。

## 参数执行规则

| 用户字段 | Switch 执行规则 |
| --- | --- |
| `ike.addr / apn / ip_stack` | `wlan.three.com.hk`、`ims`、IPv4；完整有序 IKE / ESP 提案作为权威输入。 |
| `ike.local_port=500` | IKE socket 使用标准 UDP 500 源端口，同时仍绑定通配地址、不绑定具体接口，以保留 Surge/TUN 路由。无卡探测显示随机源端口无响应，而源端口 500 收到完整 IKE_SA_INIT 响应。 |
| `reauth_interval / ims.expires` | 均为 7200 秒配置值；实际注册续期仍服从服务器确认及现有会话策略。 |
| `rf_off_delay` | 仅模块 RF 关闭后的 5 秒等待，可取消；读卡器没有 RF，不额外延迟。 |
| `rekey_pfs=14` | CHILD_SA 换钥使用新的 DH14；保留指定 ESP 套件，校验回复后才替换旧 SA。 |
| `ticket_request=true` | 首次 AUTH 发送现有 TICKET_REQUEST，不表示保证服务器发放或允许恢复票据。 |
| 两项 `cp_in_*_auth=true` | 来源实现只在首包发送 CP；final 是 first 未发送时的后备位置，不能误解释为重复发送。 |
| `cookie_payload_type=sa` | 按来源的 COOKIE 报文编码兼容选项处理；只对本模板启用，不改变其它运营商默认编码。 |
| `eap.identity_source=isim` | 完整 ISIM 身份同时用于 IKE IDi、EAP 与 IMS；没有 ISIM 时保留 USIM 派生路径，部分身份缺失明确报错。本次实卡日志显示 EAP 实际走了 SIM 派生身份（未读取到完整 ISIM tuple），因此 3HK IMS 身份是否符合卡片运营商配置仍需确认。 |
| `initial_authorization=eap_direct` | 初始 REGISTER 复用本次已验证完整 EAP 的 RES；nonce 为 RAND 与 AUTN 各前 16 字节拼接的 Base64；Digest 使用 RES 原始字节，算法标签 AKAv1-MD5。不再为初始 REGISTER 重跑卡片 AKA。 |
| IMS 安全与 Contact 能力 | 保留来源中的 `sec_agree_mode=off` 与 `initial_authorization=eap_direct`；将 `contact_features` 调整为完整语音/短信能力，并通过本地扩展 `allow_plain_ims_transport=true` 明确允许 SIP 只经已认证的 SWu/ePDG 隧道传输。状态会标记为 `swu_tunnel_only`，不会伪报为 IMS `ipsec-3gpp`。 |
| REGISTER 策略 | 保留状态码列表；用户模板显式列出的 `initial_reject_fallback_status_codes` 现在会启用对应的首次拒绝后探测回退，并依序尝试 5 种注册变体。其它配置仍保持默认关闭。 |
| E911 URL | 仅保存模板元数据，不自动发起 entitlement 请求或宣称服务已开通。 |

`eap_direct` 不能降级成空 AKA 占位。缺少本次认证材料必须明确报错；旧会话材料不能跨 Profile/代次沿用。后续 401 保留原有按服务器新挑战认证的流程，没有另加来源未实现的同 nonce 缓存重用。

## 验证与限制

模板保真、精确匹配、会话隔离、配置优先级和 Rust→Go 2722 项运营商参数回放已通过。全量 Go race 测试的既有记录为 2496 个含子测试通过、17 默认跳过；本轮改动后 `go test -race ./cmd/proletarius-macos`、`go test -race ./internal/vowifi/imscore` 与 SWu `go test -race ./pkg/swu` 均通过。完整 Rust workspace 的既有记录为 1138 通过、0 失败、14 默认忽略。忽略/跳过项不是本轮全部验收的声明。

首次实卡测试（头部修复前）在 macOS 原生 HK SIM 热点直连网络、系统代理关闭的情况下进行。EAP-AKA 与 IKEv2/ESP CHILD_SA 成功，网侧分配了隧道地址及两个 P-CSCF；本次 EAP 身份日志为 SIM 派生身份，未使用完整 ISIM 身份。REGISTER 变体均收到 403 Forbidden。由此可排除该次失败是 ePDG 不可达、代理分流或本机 TCP 建连失败；尚不能仅凭 403 区分 IMS/VoWiFi 套餐未开通、卡片 IMS 身份/域不匹配或网侧注册策略拒绝。SIM 实名不等同于 IMS/VoWiFi 已开通。没有发短信、切卡或写卡。后续头部修复后实测见下节。

纯 Rust 原生 VoWiFi 会话仍未对外启用；其后续启用门禁必须消费这些兼容字段或明确拒绝不支持的模板，不能静默使用旧 Apple 标量配置。本轮只完成当前生产路径兼容，Go 仍是执行依赖，全 Rust 迁移进度不因此提高到完成。头部修复前的实测被网侧拒绝，当时未生成 DMG。

## REGISTER 头部映射修复与复测

后续实卡复核确认，Switch 曾把用户模板中的 `include_p_access_network_info=true` 错误映射为 IMS REGISTER 的 `include_pani_authenticated=true`，进而发送 `P-Access-Network-Info: ...;network-provided`。上游把通话侧 `include_p_access_network_info` 与 IMS 注册侧 `include_pani`、`include_pani_authenticated` 分开；固定 3 HK IMS 模板将后两者都设为 false。Switch 现将 REGISTER 的“是否带 PANI”与“是否带 network-provided 参数”分开表示，用户文件未提供 PANI 字段时不发送该头；通话侧字段不再改变 REGISTER。原有 `Cellular-Network-Info` 行为保持不变。

新增请求构造测试覆盖：不发送 PANI、发送无认证参数的 PANI、发送带 `network-provided` 的 PANI；有效配置测试同时断言 3 HK 通话侧开关仍为 true、REGISTER 两项为 false。`go test ./...` 全部通过（临时目录显式置于 `Switch/build/tmp/go-test`）；Debug app 构建与深层签名验证通过。

更新后的 Debug 构建在同一张 454/03 SIMworld 卡上再次建立隧道。实际 REGISTER 日志显示初始请求 `include_pani=false`、原有 `include_cellular=true`，P-CSCF 返回 401 challenge；认证 REGISTER 返回 200，注册租期 7200 秒。界面显示“IMS 已注册，SMS 通道尚未就绪”，所以只确认 REGISTER 的 403 已由请求头映射修复消除，短信业务仍未验收；本轮未发送短信、未切换或写入 Profile。测试会话已关闭，Debug 后端进程已退出。第一轮退出后的界面状态查询曾重新拉起一次 Debug app，并短暂使用默认 Application Support 路径；发现后立即退出，没有打开短信页面或对卡执行操作。成功复测期间保持了隔离数据路径直到 app 完全退出。

## REGISTER TCP 关闭后发送失败（2026-10-07）

用户报告安装包偶发 `write tcp <local>:5060: endpoint is closed for send`。这表示 REGISTER 写入时 gVisor TCP 端点已不能发送；针对该错误，认证/初始 REGISTER 现只重连一次，并在新连接重建 Via branch 与递增 CSeq 后重试。其他写入错误不触发自动重试，重连失败会同时保留原始写入失败与重连原因。新增回归测试模拟旧端点关闭，验证新连接接收 REGISTER 且事务号更新。`go test ./...` 通过。

新 Debug 构建在当前 SIMworld 454/03 卡上真实建立 VoWiFi 隧道，界面进入“IMS 已注册，SMS 通道尚未就绪”；这次连接未触发关闭端点重试分支，因此不能声称对运营商该故障的真机复现已完成。测试结束后已关闭 VoWiFi、退出 Debug App，并确认其后端进程退出；数据只写入隔离路径 `build/runtime/ims-closed-send-test`。未发送短信、切换或写入 Profile。

修复源码已按 2.83/build283 完成 Release 构建，签名及 DMG 校验通过。安装包：`dist/Switch-2.83-macOS27.dmg`；SHA-256：`a114e3d61f510f28760e8e5d10f517f7a29c9e2e7f0d122c25e0dd8e0120a946`。保留在 `dist/archive/` 的归档与发布清单均由发行脚本生成。

按用户此前指令，IMS REGISTER 实卡验证返回 200 后已完成正式 Release 打包：`Switch-2.82-macOS27.dmg`，SHA-256 `2ea5456b62bc77e9e0bb2b874e808163827350908b1013a27234108f2985dec1`。DMG 和本地归档均通过 `hdiutil verify`/SHA-256 校验。这个发行包包含 REGISTER 头部修复；SMS 通道仍未就绪，不能据此宣称短信收发已验收。

## SIM World 语音与短信能力修正（2026-10-07）

实卡状态显示：IKE/EAP 和 IMS REGISTER 已通过，但来源模板的 `sec_agree_mode=off` 使 inner IMS `ipsec-3gpp` 不可用，短信路由资料已存在、IMS transport runtime 未就绪；`contact_features=sms_only` 也会向网络声明终端不接听语音。严格要求 inner IMS IPsec 的 A/B 得到 P-CSCF `401`，挑战不带 `Security-Server`，按预期失败关闭。当前实现保留来源的 EAP-direct / sec-agree-off 行为，只对该 3HK 模板显式启用外层 ePDG 隧道上的 SIP TCP；它要求 SWu 隧道已认证、预先绑定 Port-S、持续使用 REGISTER 的同一 TCP 连接，并将有效安全模式明确报告为 `swu_tunnel_only`。Contact 改为音频、SMS 与 MMTel。是否能实际拨号、来电和发短信仍以本轮真机复测为准；此模式的 SIP 在运营商 ePDG 边界之外没有额外的 UE—P-CSCF IMS IPsec，状态不会把它误报为 `ipsec3gpp`。

## 2026-10-08：读卡器 IMS USSD（USSI）

新增生产 imscore 的移动发起 USSI 路径（TS 24.390），使用现有已认证 IMS TCP 通道，不创建音频/RTP，也不调用 voiceclient 的旧 USSD 占位接口。INVITE 使用 `user=dialstring`、转义 `#`、multipart SDP（媒体端口 0）及 `application/vnd.3gpp.ussd+xml`；接收 INFO 菜单、发送 INFO 回复、接收 BYE 结果，并提供显式取消。

读卡器号码旁新增 USSD 查询入口；后端 `POST /api/reader/vowifi/ussd` 使用既有应用认证和 reader/Profile generation 校验。默认查询码为用户指定的 `##107#`，只有点击查询才发送。单个 IMS runtime 只允许一个查询，不自动重试，不自动订阅或启用月度任务。交互等待总上限 120 秒，每次 HTTP 等待上限 30 秒。仅支持移动发起；网络主动 USSD、新增注册能力宣告、运营商私有扩展不在当前已验证范围。Go 仍执行生产 IMS，本功能不代表全 Rust 迁移完成。

本地 race 测试覆盖 XML/multipart、号码 URI 转义、菜单交互、最终文本、拒绝响应、取消、快速 Port-S 响应和对话身份隔离。真实 SIMworld 查询结果待下面的实测记录确认，不凭 SIP 200 宣称余额读取成功。

实测结果：2026-10-08 新构建在 SIMworld 454/03 注册成功、SMS 就绪后通过读卡器 USSD 弹窗提交 `##107#`，网侧返回 SIP 403。首次尝试后将 From/P-Preferred-Identity 对齐已成功呼叫的注册号码身份，补充相应测试并重建；隔离运行 `build/ussd/Switch-USSD-Test.app`（`SWITCH_DATA_DIR=Switch/build/rt-simworld`）再次查询，仍收到 SIP 403，未收到余额文本。不得将此解释为余额为零，也不能断言 SIMworld 全网不支持 USSI。未发送短信、未拨打电话、未自动选择菜单、未启用月度任务、未生成 DMG。Release 构建与签名校验通过；IMS/应用后端 race 回归通过，USSD 专项重复 10 次通过。当前产物 `dist/Switch.app`，版本仍为开发中的 2.86。

### USSD 403 请求头复核

2026-10-08 对照 `windloom/vohive-open` 的 `third_party/vowifi-go/runtimehost/messaging/ims_ussd.go`（只参考构造策略，不复制其实现）：SIMworld/3_hk 的 INVITE 选择 `P-Preferred-Service: urn:urn-7:3gpp-service.ims.icsi.ussd` 与 `Accept-Contact: *;+g.3gpp.ussd`，Accept 包含 multipart/mixed；复用注册返回的有序 Service-Route，没有 Service-Route 才回退 P-CSCF，不再额外插入一个路由。仍省略 PANI，仍使用注册分配的号码身份。仅在已协商 Security-Verify 时发送 sec-agree Require/Proxy-Require。

上述服务标识与路由版本在 IMS/SMS 就绪后，真正的 USSD 查询入口发送 ##107#，仍获 SIP 403。随后追加对齐来源项目的 3_hk Request-URI 形式（不含 phone-context），其它运营商保留标准构造；该兼容形式的实测结果另记。普通通话拨号现拒绝以 * 或 # 开头且 # 结尾的短码，并明确指引到 USSD 入口，避免将语音 INVITE 拒绝误当作 USSI 测试。USSD 定向 race 测试通过。

构建期间发现同时发生的 Rust call_mode_adb.rs 改动曾导致 E0004/E0615/E0063，未修改或撤销该无关工作。隔离实测应用使用已通过的 Rust 构建配合单独编译、重新签名的当前 Go USSD 后端；不能将此描述为当时最新整树构建通过。

最终兼容 URI 实测：当前 SIMworld IMS/SMS 就绪后，`sip:%23%23107%23@<home-domain>;user=dialstring` 查询仍返回 SIP 403，无余额。两轮头部/目标调整均未消除拒绝，本项仍未解决，不宣称月度余额查询可用。未产生 DMG、未外发短信、未拨语音电话。

随后整树重建成功（`build/ussd-compat-final-build.log`），`dist/Switch.app` 深层签名验证通过，前述 Rust 构建阻塞已不再出现。USSD 403 的实卡结论不变，构建成功不等同于运营商查询成功。


## 2026-10-08 USSD 最终验证与入口撤回

修正初始 INVITE XML MIME 部分的 `Content-Disposition: render;handling=optional`，恢复归属域 phone-context。新版 Go 后端在专用测试包实卡验证：SIMworld IMS/SMS 就绪后仅查询一次 ##107#，网络返回 SIP 403，Reason 为 Q.850 cause=21 / SIP cause=403，Warning 399 仅重复状态码，无余额结果。

结论：当前号卡的 VoWiFi USSD 余额通道未能建立；不能据此断言运营商全网不支持，也不能把 403 归因为确定的请求头问题。读卡器没有蜂窝回退能力，暂停每月余额查询目标，移除号码旁 USSD 入口和拨号页 USSD 弹窗。保留后端协议研究与回归测试；短码语音拨号保护保留并更新提示，避免引导用户进入已删除入口。发布 2.87 (287)，按用户要求构建 DMG。没有发送短信或拨打语音测试。
