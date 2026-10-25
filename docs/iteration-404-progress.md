# Iteration 404：USSD 响应 XML 解码迁入 Rust

2026-10-12

生产 reader-backed USSD 路径现在把 bounded USSD XML part 交给 Rust IMS codec 解码，返回 language、ussd-string 和 error-code 字段。Go 保留 SIP dialog/transaction、MIME 与 multipart 边界提取、网络 I/O、命令队列、取消和 IMS 生命周期；生产会话已注入 Rust decoder，未配置 decoder 的纯 Go parser 仅服务兼容测试路径。未新增 API route，也未改变 USSD 业务语义。

验证：USSD Rust 模块 7 tests、daemon decode operation 1 test、Go imscore 与 native bridge package tests 通过。完整 `scripts/test-migration-gates.sh` 通过：Rust daemon 1,412 passed / 3 ignored，Go 全包 tests/vet、Swift BLE 回归、carrier/USB/radio/HTTP/codec/SDP/AMR/MESSAGE parity 均通过；迁移契约 `--diff` 通过，接口仍为 346 method/path（Rust 286 / Go 60）、127 Swift call sites、361 reviewed scopes，无新增路由。日志 [`dmg-3.06-migration-gates.log`](../../build/optimization/dmg-3.06-migration-gates.log)。

P3 +0.025，累计 **2.590 / 8.0**；全项目 **17.295 / 47.5 = 36.41%**（显示 36.4%，估算误差约 ±3 个百分点）。没有真实读卡器或运营商 USSD 验收。本次 3.06/build306 DMG 已生成；它包含至 Iteration 404，但不代表全 Rust 或实机运营商验收完成。
