# Iteration 375：REGISTER TCP SIP 响应解析迁入 Rust

Rust IMS parser 现在负责生产 reader REGISTER TCP 路径的完整响应语法解析：状态行、折叠头、Content-Length、重复头及二进制正文。生产流程先由 Rust 计算帧边界，再通过现有已认证 IMS codec UDS 路由请求 Rust 解析；Go 仅验证返回字段边界并重建 sipgo `Response`，供既有注册状态机继续消费。Rust frame probe 已启用但完整响应 parser 缺失时会失败关闭，避免回退到 Go `sip.ParseMessage`。未配置 Rust frame probe 的隔离旧 transport 保留兼容 parser。没有新增 HTTP method/path。

兼容性回归覆盖合法 WWW-Authenticate 折叠头、折叠 Content-Length、重复/孤立 continuation、长度和二进制正文、粘连消息及 Go 映射。完整迁移门禁通过：[iteration-375-migration-gates.log](../build/optimization/iteration-375-migration-gates.log)。Rust workspace **1,933 passed / 6 ignored**（daemon **1,382 passed / 3 ignored**）；Go 全仓 `test`/`vet`、Swift BLE completion/manager tests、carrier、USB、radio、HTTP method/HEAD、codec、SDP、AMR 与 MESSAGE parity 通过。契约扫描 `--write-inventory`/`--diff` 与 43 项扫描器测试通过；清单仍为 343 method/path（Rust 283 / Go 60）、127 Swift 调用点、356 审阅范围。

本轮保守计入 P3 **+0.025 计划人周等值**，累计 **2.115 / 8.0**。全项目为 **16.595 / 47.5 = 34.9368%**，显示 **34.9%**（两位小数 34.94%，估值误差约 ±3 个百分点）。Go 仍拥有 TCP socket I/O、物理 AKA、IPsec 安装、注册事务和 IMS 生命周期；没有实机、运营商、短信/通话或 DMG 验收。
