# Iteration 491：原生注册回退、号码持久化与完整短信组合验证

2026-10-14。延续全 Rust 后端目标，本轮修改源码并完成离线验证，不生成新 DMG。最新已交付安装包仍为 3.30 / build 330，只包含 Iteration 490；不包含本轮新增源码。

## 源码变更

- 原生初始 REGISTER 接入既有变体政策，保留模板和环境变量的显式启用条件，以及缺省状态码与显式空列表的区别。完整且事务匹配的初始拒绝在同一 TCP 上继续，保留分帧器和早到消息；EOF 等已知传输失败必须确认原连接清理后才重连。变体和 CSeq 连续推进，Call-ID 保持，From/Via 刷新，所有重试共用原候选及总截止时间。认证、受保护请求和错误安全协商不会触发初始变体降级。
- 实际 REGISTER 成功后，使用该会话验证过的 P-Associated-URI 更新当前卡的主机号码缓存和 profile notes；运营商省略 Service-Route 时同样可提取号码。绑定 reader、代次、ICCID 和 IMSI；不向 SIM 写号码，不增加卡片 APDU。仅更新 phone，保留原键、标签及其他卡记录；规范化后存在多个同 ICCID 键时拒绝歧义。真实文件 worker 持有 FIFO 和物理租约直至完成，3 秒只限制排队准入，取消不伪称撤销已经提交的文件写入。保存失败只产生安全诊断，保留注册结果；关联号码、SIM 缓存、历史备注的来源明确区分。
- 增加完整本地短信组合：实际 Service/LiveDriver、原生读卡器租约、SOCKS、IKE、外层与 IMS 内层 ESP、REGISTER、唯一 SIP actor、公开短信发送接口及耐久账本共同运行。模拟运营商在最终 REGISTER 200 前发送短信，验证持久入库后才确认；重复入站不重复归档但仍正确确认。出站由公开发送意图驱动实际 RP-DATA，接收网络 RP-ACK；重复发送意图不二次发包。退出验证实际 consumer 完成、租约释放和 IKE DELETE，无 Go socket。

## 审阅修正

初版逐变体重新连接与旧后端通常复用同一 TCP 的行为不等价，已改为同连接显式拒绝重试。号码解析原先使用空 fallback route，可能遗漏没有 Service-Route 的成功响应，已改为使用成功会话路由并加入真实 TCP 回归。

首次全量回归暴露旧证书取消测试的同步问题：解除屏障后才开始系统 SecTrust 校验，其耗时被算入 2 秒 worker 退出证明。仅调整测试，使真实证书和签名验证先完成，再在实际 worker 返回前设屏障；保留原 2 秒、8 秒预算和取消/零 AKA 断言。模拟 UDP 对端等待实际 worker 完成后，仍执行原 80 毫秒无 EAP 检查，避免提前关闭 socket 遮蔽迟到数据。没有修改生产证书验证或放宽断言。修正后证书 7 项和完整 workspace 通过，首次失败日志保留。

注册回退、号码持久化及短信组合均完成独立交叉审阅。

## 验证结果

| 验证 | 结果 |
| --- | --- |
| Rust workspace，locked，4 测试线程 | 2627 passed / 0 failed / 6 ignored；daemon 1897 / 0 / 3 |
| 初始注册候选 / IMS connection / coordinator / profile | 21 / 11 / 25 / 26 项通过 |
| 号码持久化 / ordinary / profile notes / originating | 8 / 23 / 18 / 13 项通过 |
| 生产 Service 本地组合 | 2 项通过；双向短信另行 4 路并行 16 轮全部通过 |
| Go / Swift | Go vet 与全包测试通过；Swift BLE operation proof 与 manager 38 组通过 |
| 跨语言差分 | 12 组通过，包含 2726 项 carrier replay、USB、radio、HTTP/HEAD、codec、SMS routing、USSD 和呼叫 |
| 格式与契约 | Rustfmt 通过；45 项扫描器自测通过；342 契约（Rust 305 / Go 37）、127 Swift 调用点、431 审阅范围、51 HEAD 别名 |

日志和审阅证据位于 `../build/reports/iteration-491-*`；变更前基线为 `../build/reports/iteration-491-baseline/`，4673 个文件。比对本轮冻结基线，不覆盖既有未提交工作。

## 完成边界与下一步

本轮仅操作本地模拟卡和运营商，没有真实读卡器、卡片变更、外网短信或电话测试。完整短信组合覆盖单代会话和当前 fixture 套件，不能替代实际运营商、多代恢复、音频或证书运营商组合验收。

没有启用默认原生接管，没有移除 Go 构建或运行依赖；Go 退出审计仍为 6 类阻塞、257 个生产 Go 文件。此前已审计加权估值 38.31% 仅作为历史核算值，本轮不以测试数量或迭代数量追加迁移计分。下一步继续完整 Service 语音/媒体和续期恢复组合、N待迁移与 Go 退出，最终需完成实机验收。
