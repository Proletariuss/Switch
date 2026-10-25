# Iteration 599：CoreAudio IOProc 分阶段退休

## 变更

CoreAudio 旧实现忽略 `AudioDeviceStop` 和 `AudioDeviceDestroyIOProcID` 的 OSStatus，并在失败时仍释放传给系统回调的上下文。部分启动失败的 IOProc 也可能留在状态中，但旧 `stop()` 只在 `running` 为真时清理。设备切换时，这些行为可能导致悬空回调上下文或在旧路由未确认退休时启动新路由。

新增 `AudioIOProcRetirementLedger` 跟踪每个 IOProc 是否由系统确认启动、停止及销毁。只有停止和销毁均成功，才释放回调上下文；Stop 失败时保留该注册并跳过销毁，Destroy 失败时记住 Stop 已成功，下次只重试 Destroy。部分注册失败后仍逐项清理；尚未完全退休的条目会保留，后续 `start()` 会先重试清理并在成功前拒绝创建新路由。音频缓存仅在没有仍可能执行的回调时重置。

普通停止、部分启动失败和设备变化恢复现在都能向 UI 暴露退休错误。设备变化恢复在旧路由 Stop/Destroy 未确认时停止并显示错误，不会立即启动替代路由。成功完整退休时原有路由和音频数据行为保持不变。

## 验证

- `scripts/test-call-audio-store.sh`：通过29个使用模拟音频/API 的 CallAudioStore 生产逻辑场景。
- IOProc ledger：直接检查 Stop 失败保留上下文且跳过 Destroy、Destroy 失败后仅重试未完成阶段、未启动 IOProc 不调用 Stop，共3组状态机测试通过。
- `cargo fmt --all --manifest-path rust/Cargo.toml -- --check` 与 `git diff --check` 通过。
- Release 原子打包脚本的模拟失败/覆盖保护测试通过。
- Switch 3.45/build 345 Release 编译成功；DMG 校验、只读挂载、版本、Bundle ID、arm64、Rust 后端 payload 与 deep/strict 签名核对通过。

Rust daemon 代码在598之后没有功能改动。598完整daemon首轮为3031通过、5个环境变量缺失失败、8忽略；补齐隔离目录 `SWITCH_DATA_DIR` 后，5个原失败测试分别通过。首轮完整命令退出1，故保留该事实，不把汇总称为一次完整全绿运行。

## 证据和边界

发行包：`dist/Switch-3.45-macOS27.dmg`，49,928,254 bytes，SHA-256 `ec150bd2126f41a7b48ab0e368ef983ac80b5d54afad28065c43e03ddeee756e`。只读挂载确认版本3.45/build345、Bundle ID `com.proletarius.switch`、arm64；镜像与 `dist/archive/` 归档相同。使用 ad-hoc 签名，未公证。

测试没有调用实际 CoreAudio 硬件通话路径；SIM、运营商、SMS与拨号也未测试。SIM World入站短信按用户要求暂缓。源码与DMG不等于P1–P6验收完成：P0未闭合，P1–P4验收中，P5完善中，P6未完成。接口所有权单项为294/294；整体验收分母未冻结，整体百分比不报告。
