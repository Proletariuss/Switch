# Iteration 331：PC/SC 身份刷新采用 Rust AID 与运营商选择字段

2026-10-10。所有改动与验证材料位于 `/Users/proletarius/Desktop/Switch`。未访问实体读卡器、SIM 或运营商网络，未生成 DMG。

## 变更

- Rust `ReaderCarrierIdentity` 现在保留同一 APDU 事务中从 EF-DIR 发现的完整 USIM AID；`/api/v2/reader/identity` 将 AID、GID1/GID2 与 SPN 一并返回。
- PC/SC `RefreshUSIMInfo` 在 Go 已取得的 reader workflow、generation 和 PC/SC transaction binding 下调用 Rust 身份端点，并采用 Rust 返回的 ICCID/IMSI/MCC/MNC、GID1/GID2、AID 与 SPN。
- PC/SC 路径移除 Go `readUSIMCarrierSelectors` 中的应用列表发现、GID2 和 SPN 读取；Go `usim.New` 仍独立发现 AID 并读取/解析基础身份及 GID1。Go 仍读 EF-MSISDN，并仅在 Rust 未返回 SPN 时执行既有 PNN 名称回退；旧完整 selector helper 保留给 BLE/模块兼容路径。
- 这是身份初始化的局部生产接管。Go `usim.New` 仍读取并解析基础身份，Go 仍负责 SMSC、ISIM、MSISDN/PNN 与缓存/生命周期发布；BLE 身份 APDU 尚未迁入 Rust。没有把它描述为完整 reader 身份迁移。

## 验证

- `cargo test -p proletarius-reader`：**27 passed**。
- `cargo test -p proletariusd --bin switchd-rs reader_identity`：**9 passed**。
- Go native identity / selector 定向测试通过；Go `cmd/proletarius-macos` 全包测试通过。
- 最终 `scripts/test-migration-gates.sh` 通过：Rust daemon **1,317 passed / 3 ignored**，Rust workspace 测试、Go 全包测试与 vet、Swift BLE 38 组事件序列、2,722 carrier replay、USB/radio/HTTP/codec/SDP/MESSAGE parity 通过。验证日志：[Iteration 331 migration gates](../build/optimization/iteration-331-migration-gates.log)。
- `cargo fmt --all`、`gofmt` 与 `git diff --check` 通过。

## 迁移进度

本轮把 Rust 返回的 GID2/SPN 字段接入 PC/SC 生产刷新，并删除 Go carrier-selector helper 的对应读取；AID 由 Rust 返回供补充读取使用，但 Go `usim.New` 仍会独立发现 AID，不能算 AID APDU 已退出 Go。按 P2B 计入 **+0.025 计划人周等值**。累计 **15.420 / 47.5 ≈ 32.5%**（估算误差约 ±3 个百分点）。实机、BLE、完整 USIM/ISIM 元数据与 Go 身份初始化退出仍未验收。

下一步将 Rust 身份端点扩展到完整 metadata：EF-MSISDN、PNN、SMSC/PSI、ISIM IMPI/IMPU/Domain，并让 PC/SC 刷新不再构造 Go `usim.Card`；随后复用同一字段/错误契约接入 Rust BLE APDU transport，保留身份代次 prepare/read/commit 顺序。
