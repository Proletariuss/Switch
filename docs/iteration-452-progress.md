# Iteration 452：保留并执行运营商 IKE 证书校验要求

日期：2026-10-13。所有变更和验证均在 `/Users/proletarius/Desktop/Switch` 内；未构建 DMG、未接触实体读卡器或号卡。

## 改动

- Rust carrier resolver 将上游 `ims.ike.validate_remote_certificate` 明确传入 immutable `NativeCarrierTunnelConfig`。字段类型错误会在配置解析时失败，不会被当作缺省 `false`。
- 目前 native SWu/IKE runtime 尚未实现 ePDG 证书链和 IKE AUTH 签名验证。要求证书验证的配置现在会在启动 tunnel 之前返回 `CertificateValidationUnavailable`，早于读卡器心跳、代理选择后的 socket 打开及网络 I/O；不会将其降级为普通 EAP-AKA。
- 回归验证 Three UK `234/20` profile 保留证书校验要求，CTExcel `234/33` 未要求该策略；其余 carrier 配置与优先级不变。

## 验证与范围

- `cargo fmt --all --manifest-path rust/Cargo.toml --check`、`git diff --check` 通过。
- `cargo test --locked --manifest-path rust/Cargo.toml -p proletarius-vowifi`：182 passed；carrier template/selector 与 runtime-profile tests 均覆盖。
- `cargo test --locked --manifest-path rust/Cargo.toml -p proletariusd`：1456 passed / 3 ignored；新增 native start fail-closed test 通过。
- 这只是对未实现安全策略的明确拦截，不实现证书验证，也不证明 Three UK 可注册。Carrier 上游 e560437 同步状态不变。native lifecycle/protected IMS/live SMS-call gate 仍关闭，未达到 P3 单卡 IMS/SMS 闭环验收。
- 全 Rust 项目进度仍为 **37.99%（18.045/47.5）**；未因 fail-closed 处理增加完成度。未重建 App/DMG；Go 构建、运行和嵌入依赖仍在。
