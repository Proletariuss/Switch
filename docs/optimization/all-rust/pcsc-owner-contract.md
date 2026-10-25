# PC/SC 实体会话与完整事务契约

Iteration 098，2026-10-06。**已完成接线及离线门禁；实体读卡器验收未完成。** 本文记录同一 Rust owner 接管 Go 和 Rust 卡片调用的边界，不表示已经通过实体读卡器、运营商或长期运行测试。

## 所有权

`ServerState.reader_pcsc` 是 daemon 内唯一 PC/SC 实体句柄 owner。每个 reader 由一个有界队列和专用 worker 持有句柄；一次租约覆盖完整 PC/SC transaction。Go 保留业务排序、缓存和 BLE 协调，但不再自行打开、传输或复位 PC/SC 句柄。Rust LPA、身份读取与 AKA 使用相同 owner；跨候选 AID、下载前检查到安装、身份核对到 AUTHENTICATE 不能另开连接。

独立原生调用可申请短租约；已有 Go 业务租约必须显式携带相同能力凭证，不允许按 reader 名称偷偷另开事务。Go 数字 `reader_generation` 仍是兼容业务版本，不能代替 Rust 的 session ID，也不能与 BLE generation 混用。

## 私有线协议

全部接口为 `POST /api/internal/reader-pcsc/{action}`，仅接受内部凭证，App 凭证不足以调用。请求同时绑定当前受监管 Go 子进程的 `producer_id`；内部凭证跨重启不变也不能让旧 producer 复活。公开路径的 HEAD 兼容不适用于本组接口。

| 动作 | 主要参数 | 语义 |
|---|---|---|
| `open` | producer、reader、预分配 session ID / session sequence | 登记选中读卡器；允许空读卡器，不发送 APDU。相同请求幂等，旧序号不能替换新会话。 |
| `acquire` | binding、可选 ttl_ms | 延迟建立实体句柄与完整事务；成功才返回 accepted。默认 120 秒，上限 600 秒。 |
| `renew` | binding、可选 ttl_ms | 只延长仍有效的原租约；不能复活过期或已释放凭证。 |
| `status` | binding | 查询 live / closing / expired / released / stale，以及 terminal。closing 不是实体操作已结束。 |
| `release` | binding | 撤销后续准入，等待已提交 I/O 和原生借用者退出；真实收尾后才确认。先释放后到达的 acquire 不得复活。 |
| `transmit` | binding、operation_id / operation_seq、command_hex | 固定会话提交一次 APDU；完成结果可按同操作标识查询，不能因 ACK 丢失自动重发。 |
| `reset` | binding、operation_id / operation_seq、action | `reopen` 只重建原会话句柄；显式 `reset` 冷复位只允许已有 ESTKme 策略。 |
| `close` | producer、reader、session ID / sequence，必要时 binding | 关闭指定会话，不能关闭替换会话。关闭先于迟到 open 时同样建立拒绝旧请求的记录。 |

`binding` 包含 `producer_id`、`reader`、`session_id`、`lease_id`、`lease_seq`。APDU 请求上限 65,544 字节、响应上限 65,538 字节；十六进制请求体上限 160 KiB。正常 LPA 仍使用原短 APDU 分片，不因此更改卡片下载协议。

操作序号使用高水位与有界完成结果缓存。旧序号即使已经从缓存淘汰，也只能拒绝，不能再执行。大量下载 APDU 不应因为缓存容量固定而耗尽整个租约。原生借用与 Go 线协议序号分别计数，不能互相消耗或造成迟到请求重新有效。

## 生命周期与兼容性

- generic CCID 保持跨事务复用句柄；ESTKme 保留原关闭和 terminal capability 行为。仅首次打开尚未提交 APDU且明确返回 UnresponsiveCard 时，允许受节流的一次复苏和一次重新打开；物理发送失败不得靠复位并重放恢复。
- HTTP 取消、截止时间和租约到期首先撤销准入，不能伪装实际 I/O 已终止。阻塞系统调用和原生协议闭包仍持有责任，直到实际返回。显式事务结束失败须走确认关闭；关闭失败仍保留句柄责任。
- 退出先关新准入，保留 status / release / close 控制容量。Go 生产者实际退出后才释放其卡片能力。Rust producer 换代先撤销并等待旧 worker，完成前不得启用新 producer；超时报告收尾未确认。
- PC/SC 到期以 Rust 为准。Go 镜像只能在原生 status 确认终态后释放业务窗口与锁；RPC 不确定不能自行宣布物理租约结束。BLE 原有独立生命周期不在本轮迁移范围。
- Go PC/SC 回调持有业务锁，并使用仅在该回调期间有效的 context 能力。普通刷新、无作用域通道与回调退出后的旧 context 不能借用长租约；最终化的等待阶段同样保持隔离。
- open 的最终发布与 close 的序号记录在同一 owner 锁内核对，阻塞关闭返回后再检查当前租约和原生借用者；旧 close 只清理自己会话，不能抹掉替换会话；producer 身份与关闭序号写入同锁，防止旧进程污染新代序号。
- Go eSIM 业务与缓存最终刷新、BLE、N 待/VoWiFi 调度仍是未迁依赖；本轮不是整个 reader 功能或全 Rust 后端完成。

## 验收要求

验收要求：离线测试必须覆盖同 reader 无重叠、完整身份检查和认证共用一次事务、跨 AID 和下载复用、APDU ACK 丢失与缓存淘汰、先释放后获取、先关闭后打开、旧 producer/旧 session 拒绝、取消和到期后的真实 I/O 保留，以及事务结束和关闭失败。真实 Unix socket 验证内部鉴权与退出清理路由。Go fake RPC 验证显式能力传递、未知 ACK 的收尾和业务镜像释放。

目前已有 owner fake-I/O、身份到 AKA 同一事务、Go fake-RPC 与真实 UDS 权限/退出路由回归。完整 LPA 下载的单事务计数及原生通知遍历计数仍是后续证据补强项，不能把代码接线视为已测覆盖。

实体 PC/SC 阻塞时长、插拔、读卡器固件、profile 切换和运营商验收需单独记录。本轮不操作实体卡、不发送短信。用户已明确要求修复缺口后输出 DMG，因此本轮交付 2.81 / build 281 阶段包；不据此宣称全 Rust 已完成。

语音目标保持 EVS 优先、AMR 回退、不使用 PCMA；EVS/AMR-WB 尚未实现。9eSIM `6982` 继续排除。
