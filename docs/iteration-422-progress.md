# Iteration 422：生产外呼 PRACK/BYE CSeq 分配迁入 Rust

日期：2026-10-12。Rust 外呼 tracker 现为生产 PRACK/BYE 操作提供单调递增的本地 request CSeq；CANCEL 继续复用原 INVITE CSeq。Go 校验 Rust 返回值并负责 SIP 请求组装、网络写入及媒体/会话清理；Rust 序号耗尽时失败关闭。初始 INVITE、入站呼叫和 Rust 策略 IPC 失败时的紧急 2xx 清理仍由 Go 承担。

Rust call tracker 38 项测试、daemon outgoing route 4 项、Go 定向测试与 race 测试通过；完整离线迁移门禁通过：[iteration422-migration-gates.log](../build/optimization/iteration422-migration-gates.log)，SHA-256 `c2822d953a2f424cc71e7eb36b04f87bda5384c2242f6a200f25d5e9e4993d6a`。API census 为346 method/path（Rust286 / Go60）、127 Swift 调用点、370 review scopes；契约差分通过。全 Rust 退出审计仍有7类 Go 构建/运行/源码依赖。

本轮 P5 +0.025 计划人周等值：**0.325 / 8.0**；全项目 **17.745 / 47.5 = 37.36%**（估值误差约±3个百分点）。没有实体读卡器、SIM、短信或运营商呼叫验收；9eSIM `6982` 仍排除。

阶段发行包：[Switch-3.09-macOS27-iteration422.dmg](../dist/Switch-3.09-macOS27-iteration422.dmg)，大小 53,118,453 bytes，SHA-256 `911de23e1212cda6aff529adec3beb8558e08972014916c6932ff8010391dac4`。本机 ad-hoc 签名，未做 Developer ID 公证；DMG 校验、挂载后 App 严格签名校验、版本号和 arm64 架构核对通过。
