# Iteration 427：VoWiFi 通话状态公开路由迁入 Rust

Rust 网关现在直接处理 `GET /api/reader/vowifi/calls/status` 和 `GET /api/vowifi/calls/status`，并保留各自的 HEAD 兼容行为及错误响应语义。正常 Rust 网关模式下，Go 仅通过新增的认证内部观察端点提供实时 IMS 通话状态；Rust 对返回对象实施字段白名单、类型、状态枚举及字符串/数值边界校验后，才生成公开响应。原 Go 公共 handler 仍保留给显式 standalone/兼容启动，不再作为正常 Rust 网关的公开路由。

本轮只迁移公开查询与响应边界，没有迁移 Go 持有的 IMS/SIP 会话、socket、媒体或通话生命周期；也没有做运营商实机通话验收。此结果不能证明端到端 IMS 已由 Rust 接管。

完整离线迁移门禁通过，覆盖 Rust workspace、Go 全包测试与 vet、Swift BLE 回归，以及 carrier/USB/radio/HTTP/codec/IMS parity。定向 Rust 状态 DTO 测试、Go adapter 与 race 测试、公开 GET/HEAD 方法策略和迁移扫描器测试通过。契约差异检查结果为 349 method/path（Rust 290 / Go 59）、127 Swift HTTP 调用点、371 个审阅范围，51 个 HEAD 别名；44 项扫描器自测通过。完整门禁日志：[iteration427-migration-gates.log](../build/optimization/iteration427-migration-gates.log)，SHA-256：`e48e933838bc88e60cf74d2d2203ac5c3d49ec344c9fd90909eddae4f5b48a6a`。

按既有 P5 细粒度增量记账，本轮 +0.025 人周等值：P5 为 0.450 / 8.0；全项目为 17.870 / 47.5（37.62%，估算误差约 ±3 个百分点）。这是局部 API 接管的估值，不能替代完整通话、IMS 和媒体生命周期验收。结构退出检查仍有 7 类 Go 构建/运行依赖及 264 个生产 Go 源文件。

本轮只更新源码与迁移记录，不生成 DMG；用户刚取得的 3.10/build310 Iteration 426 DMG 不包含 Iteration 427。
