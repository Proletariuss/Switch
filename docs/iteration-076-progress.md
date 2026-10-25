# Iteration 076：Rust 接管可选 QMI 控制 owner

日期：2026-10-05。改动仅在 `/Users/proletarius/Desktop/Switch/src`；Celmux参考目录未修改。

## 结果

`switchd-rs` 现在拥有可选 QMI 控制路径的进程级 owner：私有 UDS 仅接受严格的 `open`、`close`、`probe`、`set-ims`、`operator` 动作；Rust 打开 Switch 自有 macOS libusb transport、完成 CTL sync、持有唯一传输与 IMS/IMSA/NAS/UIM service client IDs，并在 blocking worker 执行同步 QMI 请求。默认 `-backend=at` 不会打开 QMI；只有旧 Go compatibility backend 显式选择 `-backend=qmi` 时才调用 Rust owner。

Go 侧现只把 Rust 返回映射成旧业务接口所需的 QMI settings、registration 与 operator 类型；QMI USB 枚举、transport、QMI transaction 和 typed IMS/UIM/NAS service 调用不再由 `quectel-qmi-go` 实际执行。IMS settings 的显式 `false`、部分更新、Has 字段和原运营商名称偏好均保持旧接口语义。home MCC/MNC 使用之前适配的 EF-OPL 优先、IMSI/EF-AD 回退读取。

## Celmux 复用

本轮接上既有 `proletarius-qmi-wire`、`proletarius-qmi-core`、`proletarius-qmi-services` 和 Switch 专属 `proletarius-qmi-libusb`。复用了 Celmux MIT wire/client/service 层并保留来源声明；没有引入 Celmux manager、QRTR、Windows USB、数据面或应用级 VoWiFi runtime。QMI owner 放入 Rust daemon，因此避免 Go 与 Rust 同时 claim 同一 USB interface。

## 验证

- `bash scripts/test-migration-gates.sh` 通过：Rust workspace 与 `switchd-rs` 全量测试（343 项）、Go vet/全包测试、2,722 carrier replay、PCMA、SDP 和 AMR parity 均通过。
- Go 新增私有 UDS mock-server 测试，验证 open/probe/set/operator/close 请求、token、显式 false、service-presence字段和 MCC/MNC 投影；没有连接 USB 或调用 QMI 硬件。
- Rust 测试覆盖严格 DTO、显式 false 与局部更新、owner 输入拒绝、shutdown 时阻止新操作且准许认证 close；请求体预算与 route-owner 清单也覆盖全部5项 QMI 动作。
- Go QMI 回调无调用方 deadline 时使用30秒 RPC期限，与旧 QMI client 的默认 request timeout 一致；已有期限会原样沿用。
- `cargo +1.98 fmt --all -- --check`、`git diff --check` 和 `python3 scripts/check-rust-migration-contracts.py` 通过。契约清单更新至 194 项（Rust 91、Go 103）。

没有打开实体读卡器/模块，没有写入 IMS 设置、发送短信或操作号卡。QMI 实机兼容、断连后的 transport/service rebind 与回调行为、通话/VoWiFi总体生命周期仍未验证，Go 仍选择 QMI 模式并拥有更高层业务编排；因此本轮不是 Go 退出验收，也不代表 QMI 已经达到硬件可用性验收。

按计划人周权重，仅将 P2B 的离线生产 owner 接线与 mock 契约验证计入部分进展：P2B 从 25% 调至 26%（+0.08 人周），整体由 **20.5% 调整至 20.7%**，即 **9.84 / 47.5 计划人周；79.3% 待完成**，估算误差约 ±3 个百分点。没有生成 DMG。
