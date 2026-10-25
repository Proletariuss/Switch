# Switch 2.77：发送恢复、UDP 验证与安全操作记录

日期：2026-10-03，Asia/Shanghai。用户授权按项目目标继续推进；中间不生成 DMG。本批延续 [VoCat 参考方案](2026-10-03-vocat-informed-plan.md)，实施 C6 的 N待发送恢复阶段、C7 与 C8 的最小范围。SwiftUI、Rust 网关/档案、Go 硬件/IMS 架构保持。

## 固定基线与边界

- 起点为实际 Switch 2.76/build276 工作区，先验证前批最终快照无新增/变化，再冻结本批起点。不能用 Git HEAD 代表包含历史未提交改动的基线。
- 起点快照：`/Users/proletarius/Desktop/Switch/build/optimization/2026-10-03-vocat-p1-baseline/`，3,253 文件、51,254,469 字节，归档 SHA-256 `0d837de014945b34115b09905672671a60d88a72d9a42f16bb4aa6e40092841b`。
- 所有源码修改在 `/Users/proletarius/Desktop/Switch/src/`；缓存、隔离状态、日志与构建位于 Switch 内。原始 Proletarius/Boo/9.29 不变，未访问生产 App 数据、短信、凭据或日志，未启动/停止生产 App、读卡器、号卡、运营商连接或真实转发服务。
- 9eSIM `6982` 与 eSIM 下载/认证路径继续排除。未改卡片授权、N待启动传输策略、短信接收调度、通话流程或系统代理设置。
- VoCat 只用于设计参考，没有复制源码或新增其依赖；不将其 Research & Evaluation 根许可证描述为 MIT。

## 已采用改动与取舍

| 候选 | 可复现起点问题 | 本批采用 | 成本与限制 |
| --- | --- | --- | --- |
| C6 / N待发送恢复 | 发送响应丢失后仅内存保留请求；Store 重建生成新 ID，可重复提交相同逻辑请求 | 原请求先落入 Rust 原子档案，再一次性授权 Go 提交；重启查询固定 Profile、号码、正文与 ID | 新增档案字段与 API、恢复 UI、写盘阶段；换取未知结果不自动重放。设备短信账本另立阶段 |
| C7 / UDP 证据 | Associate-only 测试仍返回 `udp_ready=true` | TCP、握手、Associate、真实 DNS UDP 往返分开；只有完整证据链才能确认 UDP 往返 | 仅用户显式检测，最长总预算 6 秒；不额外延长启动，不提供内置公共目的 |
| C8 / 安全记录 | 合成号码、OTP、token、APDU、嵌套错误在 Modem/REGISTER 日志中泄漏 | 局部白名单日志投影与最多 256 项内存事件；只记录固定阶段、码、结果、代次、随机关联与耗时 | 增加有限内存/日志维护成本；不保存完整协议或任意错误文本，也不宣称全系统日志已脱敏 |

这些改动以复现的正确性与隐私缺口采用，不以未测的 CPU/延迟收益立项。不追加全面重写、数据库迁移或缓存工程。

## C6：N待固定线路发送意图

新增经 App token 认证的 `POST /api/multistandby/send-intents`、`POST /api/multistandby/send-intents/submit` 与 `GET /api/multistandby/send-intents`。复用现有 Rust ArchiveStore：0700 目录、0600 原子文件；这是权限保护档案，**没有新增加密承诺**。正文不另存 UserDefaults、诊断事件或额外 Swift 明文文件。

prepare 仅保存 `prepared`，不会入短信队列或操作硬件。submit 在同一原子持久修改内将其改为 `submitting`，只有首次成功返回 `can_submit=true`。授权响应丢失、后端重启或再次查询只能得到 `unknown`，不会自动再次授权。保存失败时不调用 Go 发送接口。

已保存但未授权的意图可由用户明确继续，仍使用原 Profile/号码/正文/ID。未知意图只查询；用户明确选择新发送才生成新 ID，并提醒可能重复。Rust queue 同时校验原意图的固定 payload；持久入队时在同一次原子修改中消耗重复正文。已知 failed/cancelled 或队列证据清理对应旧 pending ID，不能阻塞下一次用户发送，也不能删除同 payload 的新 ID。

恢复列表以稳定 key 有界分页，每页 100 条，Swift 读取所有页；查询旧 prepared 结果迟到不能复活已经 queued 的意图。查询基于当前持久消息证据，不将 HTTP 成功或 SIP 200 当作手机已收到。

**C6 仍未完成设备订阅身份和设备短信账本。** 现有设备归档同时涉及 SM 当前 SIM 与 ME 历史邮箱，不能把缓存的当前 ICCID 回填到旧消息。下一阶段需要经过验证的订阅身份、线路 epoch 与接收源绑定，再实施后端持久原子 claim、取消/重启未知恢复。预计仍按原方案 4–7 人日独立核定，不以本批前端恢复冒充设备可靠发送。完整分片/投递报告也未新增。

收尾已完成设备路径的只读盘点，记录于 [C6 设备后续边界报告](/Users/proletarius/Desktop/Switch/build/optimization/2026-10-03-c6-device-followup.md)。读卡器可复用同一次 readerSnapshot/USIM/generation，模块普通短信尚缺覆盖换卡/复位的订阅 epoch。另发现 Modem void 接收回调后删除设备消息不等待归档确认，模块 IMS 回调也未传播归档失败；这是既有路径的静态缺口，本批未改其控制行为。下一阶段先限 0.5–1 人日用失败 Fixture 证明身份、SM/ME、归档确认和硬件提交租约边界，再核定实施。不能只加前端 UUID 或当前 ICCID 字段就宣称安全，无法取得可信身份/租约时保留 unknown 并拆项。

## C7：显式真实 UDP 诊断

沿用 Rust `POST /api/v2/vowifi/proxy-probe`，增加可选 `udp_dns:{server,name}`。server 必须由用户填写字面 IP:端口，name 必须显式填写域名。未提供该字段时只检查握手，`udp_ready=false`，Associate 可独立为 true。诊断界面位于“调试与诊断 → 网络诊断 → 代理 UDP”，输入仅在内存，不导出、不保存为生产配置。

一次 A/IN 查询使用随机 ID；验证外层 relay、SOCKS 内层源 IP/端口、DNS ID、QR/opcode/TC、完整长度、问题/类型/类及记录边界。解析代理主机、并发候选连接、握手、Associate、往返和请求体处理共享 6 秒总预算。取消或控制连接退出释放候选任务和 socket，无直连目标降级。普通生产传输选择、live relay 与自动启动探测未改。

DNS 非零 RCODE 仍可证明 UDP 往返，不能证明解析成功。结果不能证明 ePDG/IKE/ESP/IMS/SMS 就绪。UI 完整证据链校验避免矛盾 DTO 绿标；结果绑定检测时的输入，修改任一输入不再显示旧配置成功。总耗时包括代理解析与候选等待，UDP 往返耗时单列。

## C8：限定事件与日志边界

Go Hub 保存当前进程最近 256 项事件，容量上限 512，按同操作相同阶段/码/结果去重；无磁盘、网络、logger 或硬件回调。关联 ID 是独立密码学随机值，随机源失败禁用该项观测。GET `/api/diagnostics/events` 经既有 App token 认证，不触发硬件操作，序号取自同一次有序快照。

界面新增“操作事件”，可按操作过滤、手动刷新与导出封闭 JSON。Swift 拒绝未知枚举、非法关联、容量、排序和版本，导出仅重新编码已知 DTO；响应附加任意字段不会转存。请求代次和取消拦截迟到结果，失败显示固定提示并保留上次快照。导出使用原子写入及 0600 权限。

本批接入网络采集的排队/等待锁/提交/完成，N待实际 SessionGeneration 的连接/就绪变化/退役，以及独立 IMS REGISTER 阶段。计划 IMS 续期、IKE 重认证使用各自固定码，不记成传输故障。没有修改串口锁、启动 gate、退役顺序或重连定时。未就绪/就绪状态变化记录一次，稳定的 1 秒检测不刷新记录。

N待准备时 generation=0，尚未分配会话；独立 IMS REGISTER 也为 0，**未将其关联到 N待代次，也未新增卡片认证观察 hook**，以保持排除路径不变。N待只记录连接总阶段，不宣称精细卡认证时间线已经实现。读卡器掉线或退出经 context 取消时可能显示“取消”，细分原因仍查既有 N待运行报告；不会从安全事件恢复发件事实。

Modem logger 去掉设备身份前缀，AT、URC、短信、USSD、身份和嵌套错误仅投影为固定类别、数值/布尔和数量。原 AT 命令、响应、URC 控制对象与错误返回保持。REGISTER/routing/logger 采用固定文案与枚举，SIP trace 只保留 transport/bytes 元数据。**其余 IMS runtime、其他模块旧 logger 并未全部改写**；交互 AT/网络原始诊断也不等于安全导出内容。

## 交叉审议与验收

三个实现域分别进行定向回归，再由其他实现者只读交叉审议，最后整合复核。审议修复了：failed/cancelled 旧 ID 阻塞相同内容新发送；矛盾 UDP DTO 误绿标；输入更改后旧配置结果残留；总耗时少算解析/候选等待；实际证据行遗漏完整链校验。各项补对应生产模型/Store Fixture。Root 补实际会话代次、稳定就绪去重/掉线恢复、计划退休分类、HTTP 鉴权与安全 DTO 迟到响应覆盖。

整合复核另发现异常 USSD 数值可绕过仅检查 Go 类型的投影。已补协议范围过滤及仅用于日志显示的索引上限；实际 ExecuteUSSD 合成队列 Fixture 验证畸形大整数的原结果仍完整返回，但日志无号码/OTP 标记。未限制实际设备索引、未改原解析值。后端重启后的事件提示也明确列表仍是上次读取结果。

定向日志：`build/optimization/2026-10-03-c6-send-recovery/`、`2026-10-03-c7-*.log`、`2026-10-03-c8/`。真实 Rust supervisor + 硬件禁用 Go 进程的 IPC 新增准备/授权、App 鉴权、固定 payload、跨重启 prepared/unknown 与进程内安全事件契约。

最终聚合门禁和完整 Release 结果见 `build/optimization/2026-10-03-vocat-p1-validation/`：

- `scripts/test-functional-optimization.sh` 通过，exit 0，197.813 秒。含 Go 转发/IMS/Modem/安全事件/运行时/SWU race，Rust 存储 20 项、UDP 19 项、归属表 4 项，十二组 Swift Store/策略脚本和 12 项实际 Rust/Go IPC 检查。核对输出中真实执行数量，未把共享 target 的基线缓存测试误算为改后结果。
- 聚合之后只有 Modem 数值日志投影和事件重启提示的小修；前者追加 `TestC8Modem` race 通过，1.019 秒。后者与全部新视图在随后完整 Release 做类型检查。没有重复运行已通过的全套测试。
- `scripts/build.sh` 通过，exit 0，60.596 秒；完整 Swift 类型检查、Rust/Go 嵌入、Release arm64/macOS 27、签名校验与原子发布均成功。该耗时是单次验收值，不是构建速度 A/B 收益。
- 产物 `/Users/proletarius/Desktop/Switch/dist/Switch.app` 已核对 Info.plist 为 App 2.77/build277，主程序、两个后端与 libusb 均为 arm64，deep/strict 签名验证通过。11 个本地 Rust crate 均为 2.77.0，锁定离线元数据通过，外部依赖解析不变。

本机 ad-hoc 签名，未公证；未启动 App，未做真实代理、收发短信、窗口交互或实机长稳验收。已有 macOS API 废弃与重复 libusb 链接警告没有在本批扩展修复。

## 保留的门槛与后续顺序

本批没有 CPU、RSS、唤醒、真实 UI 绘制、端到端短信/通知延迟、真实代理或整夜链路样本。隔离回归/一次构建耗时不能作为性能改善比例。

CPU 至少相对下降 20% 且绝对下降 0.5 个百分点；短信/队列 p95 至少改善 25% 且 50 ms；构建中位数至少改善 20% 且 5 秒，90 天节省至少覆盖工程与维护成本两倍。未测、两轮未过线、超预算或关键业务退化即停止性能改造并记录取舍。

下一阶段先完成 C6 设备身份与发件恢复的独立边界；C9 同会话 IMS 续期等待真实本端自然续期基线、三次续期与 24 小时观察，C12 刷新/SSE 等待前台/隐藏/最小化各 3×10 分钟资源与时效采样。不能用 Fixture 替代运营商验收。新导出、USSD、选网或 Bot 能力按具体使用收益另行选择。6982 永久排除。

本批最终源码快照位于 `/Users/proletarius/Desktop/Switch/build/optimization/2026-10-03-vocat-2.77-final/`，归档哈希以该目录 manifest 为准，避免文件记录自身哈希循环。捕获后验证与起点对比记录在验证目录；起点中 143 个匹配 eSIM/eUICC/download 的排除文件哈希不变，未删除源码。无 DMG，无 Git 提交/重置/清理，无原项目或生产数据改动。
