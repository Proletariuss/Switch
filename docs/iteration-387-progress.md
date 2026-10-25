# Iteration 387：REGISTER 原始 Contact 解析迁入 Rust

生产 REGISTER 成功租期路径现在将请求与响应的原始 `Contact`/紧凑别名 `m` 头字段交给既有 authenticated IMS codec。Rust 负责安全分割联系人列表、解析 `+sip.instance`、`reg-id` 和 `expires`，校验 SIP/SIPS URI、按 URI 参数规则比较绑定、处理 registrar/NAT URI 改写与绑定歧义，并最终裁决 grant lease。Go 不再为生产 Rust 回调构造候选事实；旧 Go parser 仅供 evaluator 未配置的兼容调用，生产 Rust 请求错误仍失败关闭。

Rust/daemon/Go 定向测试通过，覆盖多 Contact 绑定优先级、URI 参数顺序和大小写、quoted delimiter/escaped quote、重复参数、无效 request Contact、绑定歧义、撤销及租期回退。完整离线迁移门禁通过：[iteration-387-migration-gates.log](../build/optimization/iteration-387-migration-gates.log)：Rust workspace **1,969 passed / 6 ignored**，其中 daemon **1,392 passed / 3 ignored**；Go 全包 test/vet、Swift BLE与协议差分通过。日志 SHA256：`0a7b4728445049ba6ee19dbbfcd6c3f67d96d80ab67cb6c899b0c7c21afdb6e7`。迁移契约 `--diff` 与 `git diff --check` 通过，343 method/path（Rust283 / Go60）、127 Swift 调用点、359 审阅范围。没有实机、运营商、短信或通话验收；没有生成 DMG。

P3 +0.025，累计 **2.390 / 8.0 = 29.875%**；项目累计 **16.870 / 47.5 = 35.5158%**（显示 **35.5%**，两位小数 **35.52%**，估算误差约 ±3 个百分点）。Go 仍负责 SIP socket/事务、物理 AKA、IPsec 与 IMS 生命周期，因此这不是完整 IMS Rust 化。详见[进度复核账本](optimization/all-rust/progress-reassessment-2026-10-10.md)。
