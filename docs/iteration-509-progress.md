# Iteration 509：串口 SIM 鉴权与模块 VoWiFi 原生接线

2026-10-16。本轮补齐原生串口模块的完整 SIM 身份读取、USIM/ISIM AKA 与 VoWiFi 服务接线，复用已有 Rust IKE/ESP/IMS、短信及媒体运行时。完整软件门禁、生产编译和构建输入校验均通过；实体运营商和长时间运行验收仍待完成。

## 本轮实现

串口 actor 新增持久的模块 SIM 占用能力，绑定执行器、物理串口会话、SIM 代次和射频令牌。占用贯穿射频准备、身份读取、网络会话、鉴权及最终恢复；每次 SIM 操作另持有完整事务。周期元数据探测、短信存储访问、eSIM 操作、旧 AT 写入口和其他射频请求不能插入事务。真实 SIM 事件仍使旧身份立即失效，不能被误认为物理关闭已完成。

USB 与串口共用严格 CSIM 编解码、完整卡片元数据读取、号码/SMSC 回退、身份复核及 AKA 结果映射。保留完整 USIM/ISIM AID 和原有身份规则；只对精确 EF-DIR `6986` 读取场景保留既有兼容行为，没有扩大其他状态码的放行范围。eSIM 的 CCHO/CGLA/CCHC 权限保持独立，不能借此取得普通 SIM 鉴权权限。

模块会话现按明确的 USB 或串口物理身份准备、验证和恢复。串口选择在暂未配置或连接不可用时仍然有效，不会转用其他 USB 卡或 Go 入口。SIM 代次改变会拒绝后续鉴权；已持有的原射频令牌仍可恢复同一物理会话。网络与物理事务实际结束前，取消 HTTP 等待者或停止超时都不能提前释放资源。

上一轮只覆盖串口生命周期，本轮使串口模块实际进入共享原生 VoWiFi 运行时。这属于补齐既有能力缺口；模块 VoWiFi 公开接口此前已归属 Rust，不能因此重复计算接口迁移数量或宣称 Go 已退出。

## 验证

- Rust workspace **2,929 passed / 0 failed / 7 ignored**；daemon **2,190 / 0 / 3**。Go vet/全包测试、两组 Swift BLE（manager 39 组）和 13 组协议差分通过。
- Rust workspace 生产 `cargo check --locked --workspace` 通过，编译/链接警告保留。冻结的 **4,139 个构建输入文件**内容和集合均无漂移。
- 模块专项 **299 passed / 0 failed / 1 ignored**。新增 14 项测试；既有 USB、串口短信、eSIM 与生命周期用例一并纳入完整门禁。
- 新增真实 PTY + UDS 验证覆盖完整身份读取、USIM/ISIM 鉴权、PDU 初始化失败、禁止周期 SIM 探测、身份变化时零 AUTHENTICATE 写入、取消后资源保持、失败恢复重试、旧会话清理，以及串口不可用时禁止 USB 回退。
- 新增完整生产路径的离线通信场景：串口 actor → SIM CSIM → EAP/IMS 鉴权 → IKE/ESP/IMS 注册 → 短信接收/发送/耐久归档 → EVS 媒体收发 → BYE/DELETE → 原射频恢复和占用释放。使用模拟卡片和本机回环运营商端点，无 Go socket；不是实体运营商或实卡通话验收。
- 独立复审修正了串口响应上限与共享 USB codec 不匹配、配置退役与占用获取之间的竞态，以及工作者计数与释放之间的竞态。初次专项失败来自 USB 测试夹具的原生声明顺序，两次失败日志均保留；修正按生产顺序声明后再打开设备，未放宽断言。

证据：[门禁汇总](../../build/reports/iteration-509-gate-summary.json)、[完整门禁](../../build/reports/iteration-509-migration-gates.log)、[失败与修复](../../build/reports/iteration-509-validation-repairs.md)、[独立集成审查](../../build/reports/iteration-509-integration-review.md)、[契约审查](../../build/reports/iteration-509-contract-review.md)。审查基线为本轮实施前源码快照，未用 Git HEAD 代替当前工作状态。

## 目标进度与剩余工作

契约仍为 **300 条（Rust HTTP owner 294 / Go 6）**，54 条 HEAD 别名、127 个 Swift 调用点、487 个审阅范围。76 条契约仍有 Go 归属或条件依赖，58 个显式依赖键保持。生产 Go 文件 197 个、导入包 288 个、退出阻塞 6 类；严格 Rust-only 和 Go-free 检查尚未通过。

下一阶段处理生产监督器仍启动/重启的 Go 兼容子进程、剩余状态/路由桥和显式 QMI 配置接线，随后移除相应构建与嵌入依赖。不能只停止默认子进程却遗留需要它的功能，也不能把历史上已不可达的 Go SIM-auth 代码删除重复算作业务迁移。

旧 31.4% / 38.31% 账本仍未核平；接口比例、迭代数量或测试数量不能作为整体完成率。全 Rust 目标保持 active，只有全部保留功能、零 Go 构建/运行/嵌入依赖及规定验收均完成才标记 100%。

本轮未操作实体号卡、向外部号码发信或拨号，也未生成 DMG。最新安装包仍为 **3.42 / build 342**，包含 Iteration 503，不包含本轮源码改动。9eSIM `6982` 继续排除在本轮工作之外。
