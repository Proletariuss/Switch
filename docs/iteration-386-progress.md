# Iteration 386：REGISTER 租期优先级迁入 Rust

主 reader REGISTER 路径的请求租期选择现在由 Rust `proletarius-ims` 决定：正数显式配置优先，其次采用运营商模板正数值，否则沿用 3600 秒默认值。生产 IMS session 通过既有 authenticated codec UDS 请求该结果，并将同一结果用于 REGISTER `Expires` 请求头和成功后 grant lease 核算；Rust resolver 不可用、返回无效值或请求取消时失败关闭，不回退到 Go policy。旧的未配置 callback 调用仍保留兼容行为。没有新增 HTTP 路由。

验证：Rust IMS 与 daemon codec 专项测试通过；Go imscore、macOS bridge 定向测试通过，覆盖 override/template/default、桥接字段及无效 Rust 响应。完整迁移门禁通过：[iteration-386-migration-gates.log](../build/optimization/iteration-386-migration-gates.log)：Rust workspace **1,961 passed / 6 ignored**（daemon **1,391 passed / 3 ignored**），Go 全包 test/vet、Swift BLE proof/manager regression、载波/USB/radio/HTTP/codec/SDP/AMR/MESSAGE parity 均通过；迁移契约 `--diff` 与扫描器 **43 tests** 通过，343 method/path（Rust283/Go60）、127 Swift 调用点、359 审阅范围。日志 SHA256：`14a68ddee67d1b78310216641fd89c4f7b437182d640bf30e1e66170709668bf`。未进行实卡、运营商、通话或短信验收；未生成 DMG。

P3 +0.025，累计 **2.365 / 8.0 = 29.5625%**；项目累计 **16.845 / 47.5 = 35.4632%**（显示 **35.5%**，两位小数 **35.46%**，估算误差约 ±3 个百分点）。该切片只接管 REGISTER 租期配置优先级；Go 仍执行 SIP socket/事务、物理 AKA、IPsec 和 IMS 生命周期。
