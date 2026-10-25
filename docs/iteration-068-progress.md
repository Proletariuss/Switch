# Iteration 068：复用 Celmux QMI wire codec

日期：2026-10-05。源码参考 `/Users/proletarius/Downloads/celmux-src/` 保持只读，所有项目修改均在 `/Users/proletarius/Desktop/Switch/` 内。未连接读卡器/实体卡，没有发送短信或访问运营商网络，也未生成 DMG。

## 完成内容

将 Celmux 文件级 MIT 授权的、传输无关 `crates/qmi/wire/src/lib.rs` 纳入 Switch Rust workspace，包名为 `proletarius-qmi-wire`。该 `no_std` 编解码层包含 QMUX 与 QRTR frame、CTL/service header、QMI TLV 读写和结果码解析，没有 USB、串口、Linux QRTR 或 Windows USB 依赖。复制的源文件 SHA-256 为 `b7c7e61ccefa57fb34dc9e8d86841e96326babcff3e9db0f0188cf44eaff2cfc`，与只读参考快照一致；文件的 iniwex5/anti-rainer 双作者与 MIT SPDX 头保留，`THIRD_PARTY_NOTICES.md` 已记录来源与范围。

Switch 自有回归测试锁定与现 Go `quectel-qmi-go` QMUX 编码一致的普通 service frame 和 CTL 8-bit transaction ID，并覆盖 QRTR 扩展 service round-trip。Celmux 的 QMI core、service codecs、manager 与设备传输没有引入；Go 继续持有现行 QMI/MBIM 产品路径。

## 验证

- `bash scripts/test-migration-gates.sh` 完整通过：Rust workspace **773 项通过、0 失败、11 项忽略**；Rustfmt、Go vet、Go 全包兼容测试与 carrier/PCMA/SDP/AMR parity 全部通过，carrier replay 覆盖 2,722 个向量。
- 定向 `cargo +1.98 test --offline -p proletarius-qmi-wire` 的 3 项 Switch-owned golden/round-trip 测试通过；Go `go test ./pkg/qmi` 通过。
- Celmux 源文件与 Switch 复用文件 SHA-256 相同；本轮文件尾随空白检查通过。
- 日志：`build/optimization/2026-10-05-iteration-068-migration-gates.log`。输出含既有 `native_vowifi_session.rs` 两项 dead-code warning 和 Go 重复 `-lusb-1.0` linker warning。

本轮新增的是协议 codec，不含 Rust modem/service/transport owner，也未接入 daemon 或替换 Go 调用；因此不把它计入产品后端接管比例。总进度保持 **20.0%（9.51 / 47.5 计划人周）**，P2B 保持 22%。下步先核对 Switch 实际 QMI 与 macOS MBIM 的部署路径，再按 Go 行为和硬件owner逐个迁移 service/transport；当前通知列表 GET 仍带 ES9+ 自动投递与卡内清理副作用，不能只替换成裸读取。
