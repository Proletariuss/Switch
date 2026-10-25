# Iteration 434：通话模式启用准入迁入 Rust

日期：2026-10-13。全 Rust 后端改造持续进行。本轮将通话模式启用的公开 API 和显式确认门槛移交 Rust，保留 Go 的真实模块操作执行路径。

## 变更及职责边界

- Rust gateway 现在处理 `POST /api/call-mode/enable`，检查方法、查询参数、1 MiB 请求体上限、严格 JSON DTO 和 `confirm=true`。缺少确认时直接返回兼容的 400 错误，不发起 Go 内部 RPC。
- 确认后 Rust 只调用固定的认证内部 `POST /api/internal/call-mode/enable/start`，并验证成功状态响应。Go 删除公开启用路由，只接受内部令牌保护且字段严格的 `confirm_adb_authorization` DTO。
- Go 保留实时呼叫状态复核、操作锁、射频/USB 预约、模块身份核对、配置备份、ADB授权、USB与IMS/VoLTE配置、复位、重连和恢复。没有将该工作流描述为端到端 Rust 接管。

## 验证

- Rust 新增合成 UDS/HTTP 测试：显式确认通过时仅调用固定 start adapter；`confirm=false` 时返回原错误且不存在 Go socket 也能拒绝，证明不会触达硬件。
- Go adapter 定向测试验证缺少内部令牌、未知/缺失字段及尾随 JSON 均失败关闭；Go HTTP method parity 测试通过。
- `scripts/test-migration-gates.sh` 通过：Rust workspace 合计 2065 passed / 0 failed / 6 ignored；Go 全包 test/vet、Swift BLE regressions、carrier/USB/radio/HTTP/codec/IMS parity 均通过。最终日志 SHA-256：`2c6cd556114f94e950686babbfce020379a758cad0420e1d21ca605464ef4938`。`git diff --check`、契约 `--diff` 通过。
- API census：352 method/path（Rust 291 / Go 61）、127 Swift 调用点、376 reviewed scopes。Go exit audit 仍有6类阻塞、266个生产 Go 文件。新增启用路径有明确 Go 硬件执行依赖。
- 没有执行真实模块复位、ADB授权或实体设备/运营商测试。没有生成 DMG。

## 进度与产物

本轮计入 P2B +0.025 计划人周等值，累计 **17.995 / 47.5 = 37.88%**（估算误差约 ±3 个百分点）。这反映公开请求与确认准入迁移，不代表设备工作流已切换。当前 3.13/build313 DMG 冻结在 Iteration 433，不包含本轮源码。
