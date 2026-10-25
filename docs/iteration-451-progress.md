# Iteration 451：把 3 HK EAP-direct 凭据接入 Rust 初始 REGISTER

日期：2026-10-13。所有源码与验证产物均留在 `/Users/proletarius/Desktop/Switch`。本轮继续全 Rust 后端迁移；没有生成 DMG、连接实体读卡器、变更号卡、发送短信或连接运营商 IMS。

## 改动

- Rust 读取 3 HK `user_templates/3_hk.json` 的 `initial_authorization: eap_direct`，并保留这一显式策略，不再被 `sec_agree_mode: off` 覆盖成空 Authorization。限定于 `apple_hutchison_hk` 且 home PLMN 454/03 的现有用户模板范围。
- 初始 REGISTER 编码使用同一 IKE/EAP-AKA 会话已验证的原始 RES，以及其对应 RAND/AUTN，通过 Rust AKA digest builder 生成 Authorization。没有第二次 AKA；RAND/AUTN、RES 和生成的授权头均由零化类型持有。
- 凭据从 `NativeTunnel` 一次性转入 `NativeImsNetworkControl`，随后经 generation-bound registration coordinator 传至 REGISTER builder。缺少材料或输入无效时，在发送前以 profile 错误失败关闭；传统 registration API 和其他运营商继续无凭据调用。
- 增加测试覆盖 3 HK 策略解析、缺少凭据失败关闭和首个 REGISTER 的计算 Authorization；既有 3 HK header/override 测试改为提供经过验证凭据。

## 验证

- 完整迁移门禁通过：Rust workspace **2,084 passed / 0 failed / 6 ignored**；Go 全仓 `vet` 与兼容测试；Swift BLE 操作证明和 manager 虚拟时序组；carrier、USB 状态、radio、HTTP/HEAD、IMS/USSD/语音与 SMS/MESSAGE 差分全部通过。日志：[iteration451-migration-gates.log](../build/reports/iteration451-migration-gates.log)，SHA-256 `267e1adb2f1b0a86e734d80aa6e82a91d0f5eaab23c7015d92fc67c5b94fccc8`。
- 当前源码 Release `Switch.app` **3.15/build315 arm64** 构建成功，内嵌 Rust daemon 编译完成，App 深度严格签名验证通过。日志：[iteration451-release-build.log](../build/reports/iteration451-release-build.log)，SHA-256 `0904903141b4329d1e988a5f20dfe3257d79fd2e05d2e3e8dbc120db78d5ba69`。没有将它打成新的 DMG。
- 定向结果：`proletarius-vowifi` **182 passed**、2 个 carrier integration tests 通过；native session tests **52 passed**；`cargo check -p proletariusd` 通过，仍有 17 项 dormant-code warnings。
- 契约人工审阅更新后 `check-rust-migration-contracts.py --write-inventory` 和 `--diff` 通过：**354** method/path contracts（Rust 293 / Go 61）、127 Swift HTTP 调用点、378 review scopes。扫描器自测 **44 passed**；`git diff --check` 通过。

## 迁移边界与进度

这使 3 HK `eap_direct` 从捕获结果走到 Rust REGISTER 编码器，但 native lifecycle actor、生产 protected IMS runtime 和 live SMS/call runtime 仍未开放；项目生产 readiness gate 仍关闭。本轮没有 ePDG/IMS 实网、REGISTER 响应、SIMworld 403、短信、通话或真机验收，因此不宣称 3 HK 已可注册，也不计 P3 的端到端迁移分。全项目加权进度维持 **37.99%（18.045/47.5）**；354/293/61 的 route census 不是完成率。Go 构建/运行依赖仍存在；全 Rust 后端目标未完成。9eSIM `6982` 继续按用户要求排除。

上一轮用户要求生成的 [Switch-3.15-macOS27-20261013.dmg](/Users/proletarius/Desktop/Switch/dist/Switch-3.15-macOS27-20261013.dmg) 在本轮源码修改前构建，因此不包含 Iteration 451；本轮遵守迁移期间不生成中间 DMG 的约定。

## 下一步

在不开放生产 gate 的前提下，补齐 Rust native IMS 初始注册、401/407 challenge、IPsec/security agreement、SIP response demultiplexing、刷新/注销及取消清理的同一代次闭环测试；随后再验证真实 Go compatibility owner 的替换顺序。只有完整单 Profile SMS IMS 在线闭环通过离线与指定硬件验收，才继续开放后续 lifecycle gate 并计 P3。
