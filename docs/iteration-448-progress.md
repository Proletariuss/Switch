# Iteration 448：将 3 HK User-Agent 接入 Rust REGISTER

日期：2026-10-13

## 改动

- Rust native IMS profile 从同一份 3 HK 用户模板读取 `ims.user_agent`，仅对 `apple_hutchison_hk` + home PLMN 454/03 生效。
- profile 编译的 REGISTER 现在发出模板中的完整 `User-Agent`，同时继续按模板发送/省略 P-Access-Network-Info。
- 已显式配置 `device_model` 时保留既有优先级，继续生成 `iOS/26.6 <model>`；454/04 和其他运营商保持原 User-Agent 默认值。
- 将 3 HK PANI 与 User-Agent 模板读取合并为一个严格解析步骤。无效类型或非法 header 字符会在 profile 准备阶段失败，不进入网络请求。

## 验证与范围

- Rust `proletarius-vowifi`: **181 passed / 0 failed**, 2 项 carrier 集成测试通过。
- 新回归验证 454/03 模板值进入真实 REGISTER wire、PANI 头被省略、454/04 未套用、显式 carrier override/device model 生效。
- Native session owner tests：**52 passed / 0 failed**；`cargo check -p proletariusd` 通过，保留 17 项既有 dormant-code warning。
- Go carrier 测试、Rustfmt、`git diff --check` 和 API 契约 `--diff` 通过；契约数量未变（354 contracts、127 Swift 调用点、378 review scopes）。
- 生产 IMS owner 仍 gated；没有进行实体 SIM、运营商网络或真实 REGISTER 测试。本轮不宣称 403 已修复或 VoWiFi 已注册。
- 总体迁移仍 **37.99%（18.045/47.5）**，不因未生产接管的模板字段计入完成率。未生成 DMG。

## 下一步

继续将 `3_hk.json` 中其余可由 Rust native runtime 安全表达的 REGISTER 行为逐项映射，并对照 Go wire fixtures 与 Clarivate 参考，优先补充初始授权、Contact/身份头和 retry policy。确认 Security-Server/IPsec 参数、MESSAGE/CALL 事件分派与关闭回滚的 owner 后，才评估开启任何生产 IMS gate。
