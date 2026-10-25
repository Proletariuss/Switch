# Iteration 067：USB/PCSC eSIM 通知重试迁移到 Rust

日期：2026-10-05。工作仅在 `/Users/proletarius/Desktop/Switch/` 内进行；Celmux 参考源码保持只读。没有生成 DMG，也没有访问真实读卡器、实体卡或运营商网络。

## 本轮完成

`POST /api/reader/esim/notifications/retry` 现在对选中的 USB/PCSC 读卡器进入 Rust handler；BLE 与不支持的 reader 仍完整转发给 Go。Swift 既有请求和结果结构不变：请求仍携带 `sequence_number` / `aid`，成功响应仍包含 `message` 和 `sequence_number`。

Rust 通过已经复用的 Celmux-derived `proletarius-lpa`、`proletarius-euicc` 与 Switch PC/SC adapter 读取指定 eUICC 通知，并沿既有 ES9+ TLS 信任根和直连策略发送。行为对照 Go 原实现：投递最多 4 次总调用（首次加 3 次重试），等待依次为 300/600/1200 ms，每次 HTTP 超时 30 秒；投递确认成功后才清理 eUICC 通知，清理也最多 4 次总调用并用同样的指数退避。卡片在确认时返回 NotFound 按原 Go `ErrNothingToDelete` 规则视为幂等成功。投递失败绝不清理卡内记录；租约在投递中途失效时回报结果未知，不自动再发，以免把可能已送达的通知盲目重复发送。

这项只读卡通知操作通过新的私有 `card_operation_only` 租约占用已有 Go reader operation gate / `authMu`，维持多待冲突规则，但不调用 `stopVoWiFi`，也不要求当前 SIM 身份缓存。Go 仍负责租约管理与释放；Rust 负责 PC/SC APDU、ES9+ 调用、重试与卡内确认。eSIM operation 冲突查询仍通过现有 Go 兼容接口，服务状态或租约不可用时不会继续访问卡片。短信和通知内容不进入持久日志。

## 验证

- Rust workspace 完整测试、Rustfmt、Go vet 与 Go `./...` 兼容测试通过；Rust/Go carrier replay 与 PCMA、SDP、AMR parity fixture 通过，carrier replay 覆盖 2,722 个向量。
- 定向 Rust 测试：`proletarius-reader` 26 项，`proletariusd` 336 项通过；新增用例覆盖传输重试、投递失败不清理、确认失败的未知结果、租约中断、卡片 NotFound 幂等和原有响应 DTO。
- Go `cmd/proletarius-macos` race 测试通过，新增 lease 用例覆盖无身份卡片操作租约、多待占用冲突以及释放后恢复 gate。
- API census：188 个契约，Rust handler 85 个、Go handler 103 个、127 个 Swift HTTP 调用点、65 个 fingerprinted review scopes。16 项 contract scanner 自测通过。
- `cargo check --locked --offline -p proletariusd` 与 `git diff --check` 通过。构建仍报告 `native_vowifi_session.rs` 两条既有 dead-code warning；Go linker 有既有重复 `-lusb-1.0` warning。
- 日志保存在 `build/optimization/2026-10-05-iteration-067-*.log`。

## 进度与剩余工作

只将手动通知重试 route 的 Rust 执行和离线失败边界计入 eSIM/LPA 工作包：18%→19%（+0.055 计划人周）；P2B 硬件/LPA 包保持 22%。按 47.5 人周中值权重累计为 **9.51 / 47.5 = 20.0% 已完成 / 80.0% 待完成**，估算误差约 ±3 个百分点。路由统计不计入完成率。

尚未退出的依赖包括 Go 的 reader operation lease / reader manager、eSIM operation 冲突来源、VoWiFi 与多待实际并发仲裁、Profile 通知列表、BLE 与其他 reader 的兼容路径、以及硬件结果核验。离线假服务通过不等于 eUICC、ES9+ 或运营商实测通过。9eSIM `6982` 继续排除。后续继续迁移其余 eSIM reader 行为，再推进 IMS/VoWiFi、模块和完整语音，直到全功能无 Go 构建与运行依赖验收完成。
