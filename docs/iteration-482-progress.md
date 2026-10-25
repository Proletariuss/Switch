# Iteration 482：原生 IMS 组件收敛与 3.22 发行

普通读卡器 VoWiFi 的 Rust 重构继续推进，已补齐准备上下文、共享注册 actor 下的短信与通话组件，以及隧道 UDP 和媒体退役处理。公开普通 IMS、N待会话仍走现有 Go 生产路径；三个原生接管门控保持 false。本轮不计作普通 IMS 接管完成，也不增加 Go 退出进度。

## 已实现与边界

- 准备上下文冻结运营商模板、YAML、自定义 ePDG/端口、ISIM、IMEI 和短信身份。诊断入口保留只读 IMEI、代理选择；有副作用的初始化及有界代理恢复只供 live preparation 使用。
- 出站短信使用直接类型化事务、共享 CSeq 与报告归属，支持分片、部分接受、未知结果及迟到报告。缺少 SMSC/target 只拒绝发送，不阻断注册；未知结果不重放。旧 Go 对原生已持有会话的修改在实际状态锁内拒绝，现有 Go/N待会话保留。
- 原生通话组件支持 SIP 对话、USSD、隧道 RTP、Swift PCM 和 DTMF，遵守 EVS 优先、AMR-WB 兼容策略。UDP 正常退出、错误和任务取消均在实际 socket 移除后确认。共享 dispatcher 的通知背压仍执行超时检查，并保留待投递事件和计时失败。
- 读卡器 IMS 通话历史有独立原生归属和有界耐久队列，Go 重启不能清空已持有的原生状态。实际通话结束、媒体排空及历史排空仍是释放条件。
- CHILD SA 换钥的主动、对端、PFS、响应重试及旧 SA 删除经过离线加密回环验证。IKE SA 换钥目前只有未接入运行时的材料处理；AUTH_LIFETIME 动态调度和 jitter 仍未完成。

## 验证与交付

完整迁移门禁通过：Rust workspace **2314 passed / 0 failed / 6 ignored**，其中 daemon **1660/0/3**；Go 全包 test/vet、Swift BLE **38 组**回归、**2726 项** carrier replay 和跨语言协议差分通过。契约人工复核后盘点仍为 **340** 条路径（Rust 304 / Go 36）、127 个 Swift 调用点和 387 个审阅范围。计数不换算为迁移百分比。本轮没有实机、卡片、运营商、短信或呼叫验收。

按用户要求生成 **Switch 3.22 / build 322**，arm64、macOS 27.0+：

- DMG：`../dist/Switch-3.22-macOS27.dmg`，53,395,328 字节。
- SHA-256：`fb269693760129d22860c5afd2fcb35e1401772c5311be6d67c756b51ce14215`。
- Release 构建、镜像校验、只读挂载、签名深度严格验证通过；构建目录、dist 和镜像中 30 个 App 文件完全一致，13 份许可文件齐全。
- 本机 ad-hoc 签名，未公证；仍包含 Go 运行依赖。没有安装或启动 App。

证据：`../build/reports/release-3.22-{migration-gates,contracts,build,package}.log`、`release-3.22-source-files.json`、`release-3.22-verification.json`。构建前冻结 4607 个源码/仓库文件，打包后核对无漂移；发行后仅补本文、README 和源码记录说明。

## 后续工作

继续完成普通单 Profile 的真实生命周期、成功 REGISTER 身份与通话配置、USSD/发送取消公开控制、续期恢复及租约释放接线；补全 IKE 换钥和动态寿命，再开展集成与实机等价验收。满足这些条件后才开启生产原生 IMS 门控并移除相应 Go 路径。前次加权估值 38.31% 保留为历史审计值，本轮未重新核算。9eSIM 6982 继续排除。
