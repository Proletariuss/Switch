# Iteration 069：复用 Celmux QMI IMS/IMSA 编解码

## 完成内容

从 `/Users/proletarius/Downloads/celmux-src/crates/qmi/services/src/ims.rs` 采用 MIT 授权的 IMS/IMSA service ID、TLV 字段布局和 IMS settings 编码顺序，新增 `rust/crates/proletarius-qmi-services`，依赖上一轮的 `proletarius-qmi-wire`。参考文件 SHA-256：`7adde3796c4f76bc58bd611ccda1d14c20ca851d66bd8093bfb003f47956abc3`。Rust 现可编码 IMS get/set、IMSA registration 查询，解析 IMS settings 与 IMSA registration 响应和两类状态 indication，并校验 QMI result TLV、服务号、消息号和 indication 标志。

保持 Switch 当前 Go 语义：显式 `false` 保持为值为 0 的 TLV；Go 当前不返回的 Get-IMS call-mode/presence 等字段不擅自补出；IMSA registration state 必须存在，错误码、错误文本和技术类型按可选字段保留。没有复制 Celmux 的 QMI core、manager、Linux QRTR 或 Windows USB transport。来源头、SPDX 与完整 MIT 条款已在源文件和 `THIRD_PARTY_NOTICES.md` 记录。

## 验证

- 新增 `go_ims_compat.rs` 的 7 个行为测试通过，覆盖 Go 字段/顺序兼容、false 值、服务响应错误、IMS/IMSA indications 与可选注册字段。
- 定向 QMI 测试通过：`cargo +1.98 test --offline --locked -p proletarius-qmi-services -p proletarius-qmi-wire`，共 10 项。
- `bash scripts/test-migration-gates.sh` 完整通过：Rust workspace、Go vet/全包测试、2,722 carrier replay 向量以及 PCMA、SDP、AMR parity 全部通过。输出保留既有 `native_vowifi_session.rs` dead-code 与 Go 重复 libusb 链接 warning。
- `cargo +1.98 fmt --all --check` 和 `git diff --check` 通过。

## 边界与进度

新 codec 还没有接入 `switchd-rs`，USB 设备探测、libusb 传输、CTL/service client-ID 生命周期、事务匹配及硬件单一 owner 仍由 Go 持有。没有连接读卡器，也没有对实体卡发送 QMI 请求。该协议层和 wire crate 不计作生产设备迁移。

全 Rust 后端总体进度维持 **20.0% 已完成 / 80.0% 待完成（9.51 / 47.5 计划人周；估算误差约 ±3 个百分点）**，P2B 仍为 22%。进度只有在 Rust 生产 owner 接管并通过相应行为/设备门禁后才增加。9eSIM `6982` 仍排除；本轮未生成 DMG。
