# Iteration 086：Rust 运营商通知接管与跨国家配置核对

日期：2026-10-05。所有源码及构建产物保留在 `/Users/proletarius/Desktop/Switch`。Celmux 参考目录只读；未生成 DMG，未启动生产 App，也未访问真实卡片、运营商认证服务或发送短信。

## 通知流程的生产接管

选定 USB/PCSC 读卡器的 `GET /api/reader/esim/notifications` 现在由 Rust 提供。保持既有行为：自动投递启用/停用通知，全部投递成功后才在卡内确认；安装/删除通知仍显示，供用户手动处理。BLE 或未选定的读卡器保留 Go 兼容路径。

- Go 短租约在选定 reader/generation 锁下提供已发现优先的 AID 计划，Rust 补齐静态候选、保持首次 AID 优先的去重及序号倒序。这个快照不触发新的 APDU，也不停止已有 VoWiFi 隧道。
- 同一序号可能包含多个通知 payload。扩展复用的 eUICC 核心，全量提取且整批校验；手动重试也改为逐 payload 重试，全部成功才确认一次。后项失败不会在该次请求内重新投递前面已成功的项。
- 投递或卡内确认失败保留列表条目；清理后的刷新失败，仅隐藏已经确认清除的序号。多个 AID 部分成功仍返回成功项，全失败保留错误。
- 修复卡片 List/Retrieve 返回错误 choice 时被误当作空列表的行为，保留合法空列表。畸形后续记录及输出空间不足不部分覆盖结果。
- GET 的独立 supervisor 持有 acquisition、heartbeat 和 release。HTTP 请求取消只发出停止标志，卡片/网络工作在边界检查；阻塞 worker 完成之前不释放租约。只关闭本次拥有的逻辑通道。
- 保持 Go 的查询解码、首个有效 `aid`、错误参数处理、显式 AID 与 fanout 关闭错误差异及 Swift DTO。AID 在调用卡片之前校验长度与十六进制。

Go 仍是跨进程租约、选卡生命周期及 BLE 的兼容 owner。上述 Rust 路由不代表实体读卡器或网络兼容已验收，也不代表 Go 可退出。其他旧操作的生命周期仍需继续逐项迁移。

## 验证与审查

- Rust daemon **392**、eUICC **14 单测 + 3 集成**、LPA **24**、reader **26**，合计 **459 项通过**，相关 doc tests 通过。日志：`build/optimization/2026-10-05-notifications-rust-tests.log`。
- Go eSIM notification/AID、reader session/card-operation lease 定向 `-race` 通过。日志：`build/optimization/2026-10-05-notifications-go-tests.log`。首次子代理缓存环境脚本路径未命中，该次不作为隔离验收；已用绝对工作区脚本及固定缓存重新验证。
- 12 项新增 runtime 回归包含真实本机 UDS 的鉴权、原样 Go fallback、坏 AID 在 I/O 前拒绝，以及取消 HTTP 等待者时租约仍持续到 worker 退出。均使用虚拟读卡器及合成响应。
- 纯同步模块覆盖逐 payload 投递、四次尝试及 300/600/1200 ms 间隔、不提前移除、失败可见、刷新失败过滤、取消和排序去重。
- 首次综合测试的单元部分通过，但并发构建期间 reader doc test 出现依赖工件不可用；停止并行编译、全部源码稳定后重跑整组，所有测试与 doc tests 通过。未改变或跳过测试要求。
- Rustfmt、`git diff --check`、接口契约一致性及扫描器 16 项自测通过。格式化增加尾逗号后重新人工核对四个函数指纹，再刷新清单：**196 项契约，Rust owner 94，Go owner 102，127 个 Swift 调用点，76 处审查范围**。
- 两个既有 `native_vowifi_session` 未读取字段 warning 以及 Go linker 的重复 libusb warning 仍在；没有新增故障。

## VoCat 跨国家配置

按用户新增要求继续核对 VoCat 配置和 Switch 的最终生效参数，原始配置缺省字段不能直接视为兼容性缺口。审查记录与后续实现边界见 [跨国家配置融合记录](optimization/2026-10-05-vocat-carrier-integration.md)。凡仅为漫游覆盖的 PLMN、缺乏 SIM 区分条件的 MVNO 或涉及身份改写的特殊规则，不自动覆盖现有实测方案。配置导入/静态回归与实卡认证、IMS 收发及续期验收分别记录。

本轮最终新增可重跑的审计脚本及7项脚本测试、25例Go/Rust共用选择边界fixture，并完成giffgaff已有行为及原2722例全参数差分验证。生产carrier参数最终保持原样，未声称新增已验证国家支持。3HK现有Auto模型可表达optional，不能据此推断真实运营商握手已验收。

## 进度与后续

eSIM 工作包由 19% 调整至 20%，增加 0.055 计划人周等值；固定 47.5 人周口径下，总进度为 **10.562 / 47.5 = 22.2%**，估算误差约 ±3 个百分点。Celmux 复用仍只按已接入、已验证的生产行为计分；运营商数据覆盖不计作原生 IMS 迁移完成。

仍需完成 Go 租约/设备 owner、BLE、公开短信发送意图、原生 IMS/IKE/IPsec 在线闭环、N待、语音、实机验收与零 Go 构建。后续模块发送意图及 CMTI 代次风险的审查接缝已记录于 [后续迁移审查](optimization/all-rust/modem-next-cutover-review.md)。9eSIM `6982` 继续排除。
