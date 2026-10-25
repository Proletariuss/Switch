# Iteration 066：Celmux 协议复用与 eSIM Profile 启用路由

日期：2026-10-05。工作只在 `/Users/proletarius/Desktop/Switch/` 内进行；Celmux 快照只读。

## Celmux 复用判断

复核 `/Users/proletarius/Downloads/celmux-src/` 后，继续沿用已经移入 Switch、逐文件许可确认过的 CCID/eUICC/LPA 协议核心。这些组件提供 APDU、BER-TLV、ES10c Profile 操作、BPP/LPA 状态机和可注入的 ES9 transport，缩短 Rust 本地切卡和下载的协议实现。本轮没有新增拷贝 Celmux 文件，也没有扩大第三方许可范围。

复用其协议实现，不复用 Celmux 的产品运行时策略。Profile `EnableProfile` 保留 Switch 现有 USB/PCSC APDU adapter、AID/reader 所有权和 CAT_BUSY 策略：最多重试 3 次，间隔 800 ms。Celmux 的高层重试次数/等待策略、应用层 operation 管理、Linux/Windows host transport、BLE owner、IMS/VoWiFi runtime 与 UI 均未复制。Celmux 快照没有 `.git` 元数据；许可按 crate manifest 和文件头逐项核查，不能把根目录 PolyForm Noncommercial 声明泛化到所有协议文件，也不能由局部 MIT 文件推断整个应用可复制。

## 切卡路径

选中 USB/PCSC reader 时，`POST /api/reader/esim/switch` 现在由 Rust route 执行 Profile 状态校验、durable operation 状态记录、per-reader 操作锁、lease/generation 栅栏和本地 `EnableProfile` APDU。切卡 APDU 不访问 ES9+。无效输入、CAT_BUSY 重试上限和 lease 在后续 APDU 前失效均有 fake reader 回归；operation journal 只记录操作类型/状态，不持久化 ICCID 等个人识别数据。daemon 中断后的切换结果标记为未知，须刷新 Profile 状态后再决定下一步，禁止自动重放。

路由尚非完整 Go-free：Go 仍负责操作冲突查询、VoWiFi 停止、mutation lease 发放/续期/释放，以及在租约内执行 ESTKme 特定复位、重建 reader session、同步通知和读取身份；结果再回到 Rust operation。BLE 与不支持的 reader 继续走原 Go 路径。真实读卡器和 SIM 未连接；本轮没有触发真实 APDU、短信、运营商网络或 ES9+，也没有生成 DMG。

## 验证和进度

- `proletariusd` 全量测试：329 passed，0 failed。
- Go `cmd/proletarius-macos` 全包 race 测试：通过；新增回归验证 reader finalizer 执行期间 lease release 必须等待。仍有既有重复 `-lusb-1.0` linker warning。
- Rust workspace fmt、迁移契约扫描、16 项契约扫描器自测：通过。
- 源码 route census：188 项契约，Rust handler 84 项、Go handler 104 项、127 个 Swift HTTP 调用点、59 个人工审阅指纹。该数字表示入口归属，不是完成率。
- `cargo check`：通过。保留 `native_vowifi_session.rs` 两项既有 dead-code warning；切卡路径的 unused-mut warning 已修正。
- 日志位于 Switch 根目录 `build/optimization/2026-10-05-iteration-066-*.log`。

只把 Rust 本地 Profile 启用路径和 fake APDU 故障边界计入 eSIM/LPA 工作包，将该包从 16% 调整为 18%（+0.11 计划人周）；P2B 保持 22%。按 47.5 人周中值基线，累计进度为 **9.455 / 47.5 = 19.9% 已完成 / 80.1% 待完成**，估算误差约 ±3 个百分点。Go 租约/收尾、其他 Profile 操作、BLE、实体卡、运营商兼容和 9eSIM `6982` 均不计入本次完成值；`6982`仍排除。
