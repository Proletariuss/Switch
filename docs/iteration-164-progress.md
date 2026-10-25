# Iteration 164：选中 BLE 读卡器 eSIM 概览读取迁入 Rust

日期：2026-10-08。改动仅在 Switch 源码内完成；未连接实体读卡器/SIM或真实运营商网络，未触发下载/写卡，也未生成 App/DMG。9eSIM `6982` 下载问题仍按用户要求排除。

当用户选中 BLE 读卡器时，`GET /api/reader/esim` 现在由 Rust 经现有 Swift CoreBluetooth/APDU 桥读取 EID、eUICC信息/配置地址、Profile列表，以及可选的 eSTK 产品字段，并由 Rust投影为既有 JSON 结构。PKI issuer/accreditation JSON 已搬到 `proletarius-euicc` crate 自有资源；与 Go 兼容副本逐字节一致，避免 Rust 编译依赖 Go 源码目录。

每条 APDU 仍绑定选中 reader identity；Rust复用现有 generation-bound Go lease/heartbeat/finalizer 和操作快照。未选中 BLE reader 或不适用传输继续走兼容路径；桥接预检失败前可回退，开始 APDU 后的错误失败关闭。USB/PCSC、Go reader session 生命周期和真实硬件重插验收未因此迁完。

验证通过：`scripts/test-migration-gates.sh` 完整门禁；Rust workspace 中 `proletariusd` 951项通过、3项忽略；Go 全仓 `test`/`vet`、2,722项 carrier replay、USB/radio/codec/HTTP-HEAD parity；19项 Rust/Go隔离进程集成；43项契约工具自测及 `--diff`。契约仍为291条（Rust224、Go67，含37个HEAD alias）、127个Swift调用点、272个review scope。Rust-only退出门禁仍被默认Go fallback与147条Go owner/dependency route阻断。

保守计入P2B **+0.025计划人周等值**：由4.925升至**4.950/8.0（61.88%）**；总量由13.695增至**13.720/47.5≈28.9%**（估算误差约±3个百分点）。没有将fake APDU验证计为实机或eSIM服务端验收。
