# Iteration 374：REGISTER TCP SIP 分帧决策迁入 Rust

Rust IMS codec 的 `probe_sip_frame` operation 现在为 production REGISTER TCP 读路径决定 `need_more`、keepalive 与完整 SIP 消息的精确边界。Go transport 仍执行 TCP 读取，并在 Rust 给出完整帧后调用既有 SIP parser 建立对象；未解析的尾部字节保存在同一 transport，供下一次事务读取。已覆盖分段头、二进制 body、CRLF keepalive、粘连响应、Content-Length 边界、malformed frame，并明确保持 Go SIP parser 的 65,535 字节帧上限；Rust UDS adapter 对 disposition、长度和 keepalive 字节做二次边界校验，daemon 不可用时生产路径失败关闭。

完整门禁通过：[iteration-374-migration-gates.log](../build/optimization/iteration-374-migration-gates.log)。Rust workspace **1,930 passed / 6 ignored**（daemon **1,381 passed / 3 ignored**），Go `vet`/全包测试、Swift BLE/manager tests、carrier replay、USB/radio/HTTP method+HEAD/codec/SDP/AMR/MESSAGE parity 均通过；契约扫描 `--write-inventory`、`--diff` 和扫描器43项自测通过。契约仍为343 method/path（Rust283/Go60）、127 Swift调用点、356审阅范围，无新增 route。没有实机、运营商、短信/呼叫或 DMG 验收。

P3 +0.025 计划人周等值，累计 **2.090 / 8.0**；全项目 **16.570 / 47.5 = 34.8842%**（显示 **34.9%**，两位小数 **34.88%**，估值约 ±3 个百分点）。Go 仍拥有 REGISTER TCP socket/read、SIP semantic parser/object dispatch、IMS transaction/session lifecycle、物理 AKA 与 IPsec 安装；本轮只迁移 framing policy，不宣称 IMS 全链路或 VoWiFi 实机验收完成。没有生成 DMG。
