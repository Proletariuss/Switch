# Iteration 335：模块 SIM 身份读取移交 Rust

2026-10-10。所有源码、记录与构建产物仅在 `/Users/proletarius/Desktop/Switch` 内。本轮未连接或操作实体读卡器/SIM，未发送短信、拨号、切卡或下载 Profile；未生成 DMG。

## 变更

- 新增经内部 token 认证的 `POST /api/internal/usb-at/sim-identity`，只接受空请求体；Rust 在同一 USB AT admission 中按固定顺序执行 `AT+CNUM`、USIM/ISIM 身份 metadata APDU 和 `AT+CSCA?`。调用方不能提交 AT 命令或 APDU。
- Rust USB AT worker 对 identity transaction 只放行严格格式的 `AT+CSIM`、`AT+CNUM` 与 `AT+CSCA?`；CSIM 长度、十六进制请求与返回长度均校验。超时/断链不会重放 APDU，错误响应不包含卡片身份或原始卡片数据。
- Rust reader 新增模块 EF-DIR 读取策略：只跳过单条 READ RECORD 返回的精确 `6986`，其他状态仍失败关闭；必须发现完整 USIM AID。可选 ISIM 发现使用相同策略。
- Go `openModuleVoWiFiSIM` 现在只消费 Rust 的 metadata 响应来构建现有 VoWiFi 兼容会话对象；此启动读取路径不再由 Go 建立 `usim.Card` 或发送 metadata APDU。Go 仍保留 AKA、切卡后 Profile 复核、VoWiFi 会话和模块物理生命周期。
- 路由审阅清单补齐 Iteration 334 的身份缓存路由，并登记新的 SIM identity 路由；当前盘点为 341 method/path（Rust 280 / Go 61）、127 Swift HTTP 调用点、354 审阅范围。

## 验证

- `proletarius-reader`：32 项通过，覆盖仅跳过 `6986`、其他状态拒绝及普通 EF-DIR 策略不变。
- Rust daemon：1,324 项通过、3 项忽略；定向测试覆盖 AT 命令白名单、CSIM 解析与字符长度、APDU 编码及单次发送不重放，并核对 CNUM/CSCA 字段映射沿用旧解析顺序。
- Go `cmd/proletarius-macos` 全包测试通过；新增 metadata AID wire 校验与 SMSC 回退测试。
- Rust migration contract census 与其 43 项测试通过；路由输入上限为 4 KiB，契约审阅未开启 Rust-only 退出门禁。
- 完整 `scripts/test-migration-gates.sh` 通过，覆盖 Rust workspace、Go 全包/vet、Swift BLE、2,722 carrier replay 与既有 USB/radio/HTTP/codec/SDP/MESSAGE 差分。日志：[Iteration 335 migration gates](../build/optimization/iteration-335-migration-gates.log)。
- `cargo fmt --all --check` 与 `git diff --check` 通过。未实测真实模块、读卡器或运营商行为。

## 进度与边界

按 P2B 增计 **+0.050 计划人周等值**：模块模式的完整身份 metadata APDU 生产序列已由 Rust 执行，并由现有生产启动路径消费。累计 **15.620 / 47.5 ≈ 32.9%**，估算误差约 ±3 个百分点。此项只覆盖模块身份采集；Go AKA、SIM/APDU 其余消费者、SIM/reader 完整生命周期、VoWiFi/N待业务会话及零 Go 构建/运行验收仍在目标范围内。

下一步把 module SIM 身份读取接入 reader identity generation prepare/read/commit，与 PC/SC/BLE 使用同一代次快照发布协议；再把 AKA、Profile 复核与模块 SIM/APDU 仲裁从 Go 迁入 Rust。实机验收须另行按授权设备进行，不以本轮离线测试替代。
