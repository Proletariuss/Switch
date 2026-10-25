# Iteration 373：REGISTER 请求身份与响应匹配由 Rust 权威解析

生产 reader VoWiFi 注册响应校验现在接收请求帧与响应帧。Rust 从 REGISTER 请求本身校验 SIP start-line、Content-Length、唯一 Call-ID/CSeq，并以该身份匹配响应事务后分类 accepted/challenge/rejected；Go 不再先解析请求并把身份字段作为可信期望值传入；Rust 返回 typed action 与 status，Go 只核对 status 是否与 transport 解析结果一致。Go 侧 Rust validator 未配置或 socket 不可用时失败关闭。TCP transport 与 SIP stream parser 仍由 Go 持有，孤立未配置 validator 的兼容调用仍保留旧分类路径，本轮不声称 IMS transport/session 已迁完。

验证：Rust IMS crate **53 passed**，daemon IMS codec 定向测试 **27 passed**；Go `imscore` 与 `proletarius-macos` 定向测试通过。完整 `scripts/test-migration-gates.sh` 通过，日志：[iteration-373-migration-gates.log](../build/optimization/iteration-373-migration-gates.log)，Rust workspace **1,929 passed / 6 ignored**（daemon **1,380 passed / 3 ignored**），Go 全包 `vet`/测试、Swift BLE proof/manager 回归、carrier/USB/radio/HTTP/HEAD/codec/SDP/AMR/MESSAGE parity均通过。契约清单 `--write-inventory`、`--diff` 与 43 项审计器自测通过：343 method/path（Rust 283 / Go 60）、127 Swift 调用点、356 审阅范围；格式检查和 `git diff --check` 通过。

未访问实体卡、未进行运营商/短信/通话验收、未生成 DMG。Go SIP stream/socket、响应 parser/dispatch、物理 AKA、IPsec 安装及 IMS 生命周期仍待迁移。

P3 +0.025，累计 **2.065 / 8.0 = 25.8%**；整体 **16.545 / 47.5 = 34.8316%**（显示 **34.8%**，两位 **34.83%**，估算误差约±3个百分点）。
