# 2026-10-02 短信转发与优化决策

## 基线与保护

- 工作区：`/Users/proletarius/Desktop/Switch`；源码版本 2.73/build273，arm64、macOS 27.2、Xcode 27.0。Go/Rust 后端与所有构建、测试产物均留在 Switch 内。
- 功能前源码保护快照：`build/optimization/sms-forwarding-baseline/src-before.tar.gz`，SHA-256 `1cce25f9db657000fb705d24ba81be43c65f748907f408b0972a938f6f5b764a`；对应 SHA-256 清单 3,253 项。该工作区同时有未提交的既有改造，故以快照和逐文件清单作本轮基线，不把旧 Git 提交当作实际基线。
- 功能前 `src` 约 87 MiB。短信转发此前未接入应用，因此没有可用于 CPU、消息端到端延迟、失败恢复或通知投递率的运行时 A/B 样本。
- 已有门槛沿用 `2026-09-30-stability-decisions.md`：CPU 改善至少 20% 且 0.5 个百分点；短信页面 p95 至少 25% 且 50 ms；常见构建至少快 20% 且 5 秒；并需证明 90 天节省时间达到工程成本 2 倍。测试运行时间、源码行数和单次构建不作为性能收益。

## 目标与验收方法

| 目标 | 验收方法 | 结果 |
| --- | --- | --- |
| 常规设备短信与 N 待新入站短信均进入转发队列 | 对生产 Store 使用隔离 API/消息夹具；确认出站和历史记录不进入队列 | SMS Store 8 组、N 待 Store 6 组通过；无硬件或运营商短信 |
| 每条消息只转发一次，网络失败后可重试 | Go 队列测试重复入队、入站/出站筛选、持久化与 API 测试 | 通过；队列加密写入后才返回本机接受，失败保留待重试状态 |
| 机密不进入普通偏好、队列旁密钥文件或状态 API | 检查 Swift 持久化路径、状态接口响应，并测试错误密钥不能解密队列 | 通过；凭据及 AES-GCM 队列密钥由 macOS 钥匙串保存，队列文件不带旁置密钥 |
| Telegram 默认使用系统代理并允许自定义 | 检查默认设置与配置测试；Go 构建验证 macOS CFNetwork 代理解析编译 | 通过构建与代理单测；未对真实代理凭据、PAC 服务或 Telegram 账户发消息 |
| Bark 图标与加密可配置 | 解密本机测试服务器收到的固定夹具，核对图标、分组、短信正文和随机 IV | Go 隔离测试通过；未向用户 Bark 设备推送 |

## 已采用的实现

- 短信页工具栏新增“短信转发”入口，Telegram Bot 与 Bark 可分别开关；保存后才应用，Bot Token、Bark 设备密钥、Bark 加密密钥、自定义代理地址及本地队列密钥存入钥匙串。
- 配置只通过已有本机认证 Unix Socket API 送入 Go 后端；后端内存持有运行凭据，状态接口只回报启用状态、代理模式、待发送数和非敏感错误码，不回传凭据。
- 新短信在本机 AES-GCM 队列中持久化，队列密钥由钥匙串管理，不与密文放在同一个目录。每个 provider 独立记账、失败退避重试；重复 ID 去重。N 待消息携带独立命名空间 ID，不发送 ICCID/Profile 标识作为通知正文。
- Telegram 默认模式先遵循进程代理环境，再按 macOS CFNetwork 的系统网络设置解析 HTTPS 代理或 PAC 决策；系统决策为直连时才直连。还提供手动 HTTP/HTTPS/SOCKS5 代理和显式直连选项。PAC 解析失败不会回退直连。
- Bark 支持自定义 HTTPS 服务、图标 URL、通知分组、AES-128-CBC/AES-256-CBC。图标与正文一起加密，每次推送使用新 IV。
- 测试按钮发送固定的“短信转发测试通知”，不会把真实短信放进测试请求，也不会更改 SIM/读卡器。

## 优化候选与取舍

| 候选 | 决策与理由 | 重开条件 |
| --- | --- | --- |
| VoWiFi 掉线/续期策略或轮询调参 | 停止本轮优化：本轮未测读卡器、运营商与真实 VoWiFi 会话，没有按 Profile 归属的掉线及恢复样本。修改保活会改变无线/卡会话行为，收益不能用现有离线测试证明。 | 取得可归属到每 Profile 的在线、失联、续期、重连时间序列；固定设备/运营商条件重复测试，并先验证故障分类。 |
| SMS 轮询频率或前端列表缓存 | 不实施：现有 3 秒级后台通知和空箱读取已有专门回归；本轮没有同条件 CPU 或新短信发现延迟测量。为压低读取次数而延迟通知不满足收益门槛。 | 相同硬件、页面、后台时长各做至少 3 组 A/B，达到既定 CPU/p95 门槛且通知发现无回退。 |
| 构建缓存、Swift/Rust/Go 架构重写或全库重构 | 不实施：一次 Release 构建只验证交付，不构成 A/B；没有 90 天构建频率和成本回收证据。跨层重写会扩大 eSIM、N 待及 VoWiFi 回归面。 | 常见改动构建连续测量至少 3 次，并证明速度门槛和 2 倍成本回收。 |
| 加密队列密钥改为服务端旁置文件或提升队列批量性能 | 不实施：当前钥匙串密钥与密文分离、单条/批量入队和 provider 重试通过离线测试；尚无队列积压或 CPU 瓶颈数据。 | 真实队列积压复现可观测延迟，且基准证明候选收益超过加密与丢失保护成本。 |

本轮没有达到性能阈值的独立候选，因此按门槛停止额外性能改造。队列锁定、重配置串行化、持久化失败回滚属于本功能的正确性保护，不作为 CPU 或速度提升主张。未变更 VoWiFi、电话、eSIM、短信收发实现以及其他页面。

## 验证与边界

- `go test -count=1 ./internal/smsforward ./cmd/proletarius-macos` 通过；覆盖 Telegram/Bark 测试服务器、Bark 加密/图标、新鲜 IV、代理模式、队列去重、状态脱敏与钥匙分离。
- `scripts/test-sms-store.sh`：8 组生产 Store 异步回归通过；`scripts/test-multistandby-store.sh`：6 组 N 待回归通过。测试使用隔离夹具，无真实读卡器操作。
- `scripts/build.sh` 完成 macOS 27 arm64 Release 构建；`dist/Switch.app` 深度严格签名验证通过，约 70 MiB。没有生成 DMG。
- 未配置或测试用户的 Telegram Bot/Bark 账户；未向真实号码或通知设备发送内容。启用 Telegram 前，用户需在 Telegram 主动向 Bot 发送 `/start`；Telegram Bot 消息不是端到端加密消息，Telegram 会处理该消息内容。[Telegram Bot 入门](https://core.telegram.org/bots/tutorial)
- Bark 的兼容加密格式为 CBC；它保护传输到 Bark 的通知正文，但不提供消息认证码。远程 Bark 仍强制 HTTPS。[Bark 加密说明](https://github.com/Finb/Bark/blob/master/docs/en-us/encryption.md)
- macOS 系统代理决策由 CFNetwork 的代理/PAC API 解析。[Apple CFNetwork 代理解析 API](https://developer.apple.com/documentation/cfnetwork/cfnetworkcopyproxiesforurl%28_%3A_%3A%29?changes=_6&language=objc)。若系统代理依赖仅存于钥匙串、CFNetwork 代理字典未提供的认证信息，请使用自定义代理地址填写认证信息。

最终产物仅为 Switch 源码、验证记录和本地 `Switch.app`；DMG 仍未生成。
