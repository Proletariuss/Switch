# Iteration 602：陈旧模块音频启动的清理结果

2026-10-20。工作区固定为 `/Users/StatTrak/Desktop/Switch_MAC/`；源码在 `src/`，构建和测试证据在 `build/`，发行物在 `dist/`。早期目标路径 `/Users/proletarius/Desktop/Switch/` 不再作为当前写入地址。该路径已写入项目方案首页、`AGENTS.md`、本文件及进度看板。Codex 当前目标记录仍包含旧路径，且本机目标工具只提供读取及状态更新、没有修改 objective 文本的操作；因此项目内这些文件是本轮可维护的权威路径记录。

本轮修复 `CallAudioStore` 中陈旧的模块音频启动回调：当 operation revision 已过期，需要停止本机模块音频时，之前会丢弃 `AudioBridge.shared.stop()` 的失败结果。现在停止未获确认时，保留清理错误并向界面报告，不把未知清理状态当作已释放。新增模拟回归场景，强制启动回调过期并让后续 CoreAudio stop 失败，验证错误仍被保留；同时现有模块恢复、接听/挂断状态与异步完成隔离场景继续通过。

## 本轮验证

- `./scripts/test-call-audio-store.sh`：31 个异步 CallAudioStore 场景及 CoreAudio retirement ledger 检查通过。证据为 [`iteration-602-call-audio-store.log`](../../../../build/reports/iteration-602-call-audio-store.log)，SHA-256 `3fa5f5eaf03f13b01fe019f7a9d14d721d1a9d78c96e84e798765a23f6a63045`。此为 mock 音频/API 回归，不是实机 CoreAudio 或运营商通话验收。
- `check-rust-migration-contracts.py --require-rust-only`：294/294 method/path 契约的 Rust owner，Go owner 0；129 Swift HTTP call sites，470 个审阅指纹范围。静态门禁不等于完整运行时 Go-free 验收。
- `check-rust-backend-exit.py --require-go-free`：结构检查通过；不覆盖应用运行、迁移恢复或设备验收。
- `check-acceptance-ledger.py`：metadata consistent；294 API、131 Swift、56 runtime rows、115 个需求叶子、21 个场景类别；整体分母未冻结、总体百分比为空、未授予功能验收、产品未完成。
- Rust daemon 本轮没有源码变更；最新完整单次全量回归仍是 Iteration 601 的 3036通过、0失败、8忽略，日志 SHA-256 `176aceb7ff79a39a9ffa61bcd3e1ce8215cbb380c25fba7f128f61084a42e1f9`。本轮不把旧全量结果重复记为新测试。
- 当前 USB 枚举返回空设备列表；未进行实卡、VoWiFi、短信或拨号测试。用户此前暂缓入站短信，本轮没有发送短信。
- 按精确清理工具清除已完成测试后不再需要的 `build/rust-target`，回收分配空间 `7,184,171,008` 字节；依赖、源码和发行物保留。

## 当前进度与剩余工作

后端接口归属 294/294（100%）仅是单独的静态覆盖指标。P0 基线与完整来源审计未闭合；P1–P4 仍需硬件和业务验收；P5 仍需实网语音、音频路径及完整设备矩阵验收；P6 的启动恢复、升级回退、长稳和最终交付门禁仍未闭合。总验收分母未冻结，不能计算整体百分比。没有生成 DMG。
