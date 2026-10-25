# Iteration 384：成功 REGISTER 短信路由投影迁入 Rust

成功 IMS REGISTER 的响应现在通过既有 authenticated IMS codec UDS 调用 Rust，按 `P-Associated-URI` 选择短信身份，并保留 `Service-Route` 顺序；无路由时沿用当前配置生成的 fallback。Go 仍负责从 SIP response 结构提取 headers、IMS socket/事务、物理 AKA、IPsec、SIP dispatch 与生命周期。生产路径配置 projector 后，Rust 不可用或拒绝输入时失败关闭，不回退到 Go 路由选择。未增加公开 HTTP 路由。

Rust 复用 `proletarius-vowifi::message_routing::RegisteredMessageRouting`，新增 `from_headers` 入口并让现有解析响应入口复用该策略。authenticated IMS codec handler 对身份、route 数量和长度做边界检查，并清零临时输入 header。Go 桥验证响应身份及有序 route 列表；成功 REGISTER 组装使用 Rust 投影。Go compatibility helper 仍供未配置 Rust projector 的隔离测试/兼容配置使用。

验证：专项 Rust `proletarius-ims` REGISTER lease 6 项、`proletarius-vowifi` message routing 2 项、daemon codec projection 1 项通过；Go focused IMS/daemon bridge tests通过。完整迁移门禁通过：[iteration-384-migration-gates.log](../build/optimization/iteration-384-migration-gates.log)，Rust workspace 1,961 passed / 6 ignored（daemon 1,390 passed / 3 ignored），Go 全包 vet/test、Swift BLE proof/manager regressions、carrier/USB/radio/HTTP/codec/SDP/AMR/MESSAGE parity通过；契约 scanner `--diff` 与 43 项审计测试通过。门禁日志 SHA256：`c6a1a549e4e1f870b6f6d190fb5365fa549af31960cbc81dcdc6b3426a4d375e`。未进行真实 SIM、运营商、长期 VoWiFi/短信/通话测试，未生成 DMG。

P3 +0.025，累计 **2.315 / 8.0**；项目累计 **16.795 / 47.5 = 35.3579%**（显示35.4%，两位小数35.36%，估算误差约 ±3 个百分点）。本增量只计授权身份/路由投影的生产策略迁移，不计 IMS socket、完整 IMS 会话、SMS 网络投递或 SIP 生命周期接管。
