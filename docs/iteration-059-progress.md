# Iteration 059 — Celmux eUICC 通道生命周期适配

Date: 2026-10-05  
Goal: Switch 全 Rust 后端迁移；不生成 DMG。

## 本轮实现

- 按用户提供的 `/Users/proletarius/Downloads/celmux-src/` 再核对 CCID channel API。Switch 已复用 Celmux MIT `OwnedChannel`、`CardTransport` 和 CLA channel binding；本轮补齐一个贴合 Switch 既有 PC/SC 行为的安全入口。
- 新增 `OwnedChannel::open_selected`：只申请新逻辑通道并选择明确 AID；SELECT 失败时只关闭该次申请的通道，正常结束时由 owner 自动关闭。
- 新增 `proletarius-reader::euicc_transport::open_euicc_channel`，把 Switch 已有 APDU transport adapter 与所有权型逻辑通道组合起来，给即将接入的 Rust LPA/download runner 使用。
- 刻意没有使用 Celmux `OwnedChannel::open` 的全部预处理语义：它会探测/关闭 channel 1–3 并回退基本通道，而 Switch 现有 Go CCID 流程只管理当次申请的 channel。该差异已记录在 Celmux 复用评估中，避免把参考项目行为直接等同于 Switch 兼容行为。

## 验证

- `cargo +1.98 test --offline -p proletarius-reader`：22 项通过。
- `cargo +1.98 test --workspace --offline --quiet`：735 项通过、11 项忽略、0 项失败。
- `cargo +1.98 fmt --all -- --check` 与 `git diff --check` 通过。
- 新 fake-APDU 用例覆盖 open → select → CLA 绑定 → close 的完整顺序，以及 SELECT 失败时原始 SW 保真并只清理本次逻辑通道。
- 全程未连接实体卡/读卡器、未访问 SM-DP+、未下载或写入 Profile。没有处理 9eSIM `6982`。

## 影响范围与进度

这为将要切换的 LPA runner 提供了资源所有权接缝，但 `proletariusd` 尚未把 eSIM download、operation polling/confirmation 或 lease heartbeat 接到该 adapter，公开 eSIM 路由仍由 Go 服务。因此不计生产接管进度，整体仍为 **17.5% 已完成 / 82.5% 待完成（8.305 / 47.5 人周，估算误差约 ±3 个百分点）**。本轮没有改变公开路由 owner，也没有生成 DMG。

下一步仍是建立 Rust eSIM operation service：维持现有 API/错误/确认 UI 契约，绑定被选择的 PC/SC reader，持有带 generation 的 Go reader lease，执行 blocking LPA 工作并在结束时核验/释放 channel 与 lease。离线 mock 与 Go 差分通过后，再决定是否进入受控设备验收。
