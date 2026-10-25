# Iteration 449：将 3 HK REGISTER 用户模板字段纳入 Rust profile

日期：2026-10-13

## 改动

- Rust native IMS profile 现在在 454/03 且 carrier 为 `apple_hutchison_hk` 时读取 3 HK 用户模板的 `Expires`、`Supported`、`Allow`、`sec_agree_mode`、PANI 与 Accept-Contact、P-Preferred-Identity、P-Visited-Network-ID、Route 开关，以及上一轮接入的 User-Agent。
- `sec_agree_mode: off` 转为 disabled，使该 profile 的 REGISTER 不再生成 `Security-Client`、`Require: sec-agree`、`Proxy-Require: sec-agree`；模板 `Supported: path,gruu` 与 `Allow` 列表进入实际 wire 编码。
- 显式 Rust carrier override 仍后置覆盖模板；回归测试验证 `Expires`、Supported、PANI 和 device model 的覆盖优先级。其他运营商与 454/04 不应用 3 HK policy。
- 本轮没有接入 `initial_authorization: eap_direct`、IKE/SWU/IMS native session owner，也没有开放生产 gate。3 HK 配置字段进入 profile 不等于 3 HK 注册/通话/短信可用。

## 验证与范围

- Rust `proletarius-vowifi`: **181 passed / 0 failed**；2 个 carrier integration tests 通过。
- Rust daemon `native_vowifi_session::tests`: **52 passed / 0 failed**；`cargo check -p proletariusd` 通过，仍有 17 项 dormant-code warnings。
- Go carrier 与 IMS core tests 通过；Rustfmt、迁移契约 `--diff`（354 contracts，Rust 293 / Go 61；127 Swift call sites；378 review scopes）及 `git diff --check` 通过。
- 迁移进度仍为 **37.99%（18.045/47.5）**。此切片属于未接生产 owner 的 native profile，未计完成率；Rust-only / 零 Go 依赖门禁仍未通过。
- 没有实体卡、运营商网络或 live REGISTER 验收；不据此宣称此前 403 已解决。本轮未重新生成 DMG。

## 下一步

先在 Rust native IMS transaction/coordinator 中实现并验证 3 HK `eap_direct` 初始 Authorization 的获取、报文一致性和失败关闭行为，再接入 authenticated SWu/IMS session owner；随后补齐 challenge、IPsec、安全关闭/回滚和 live event demultiplexing 的全链路验收。在生产 owner 全门禁通过前保持 IMS native gate 关闭。
