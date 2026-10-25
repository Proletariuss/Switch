# Switch 3.45（构建345）

2026-10-20，Apple Silicon，macOS 27.0及以上。本机 ad-hoc 签名，未公证。

[下载 DMG](/Users/StatTrak/Desktop/Switch_MAC/dist/Switch-3.45-macOS27.dmg)。大小 49,928,254 bytes；SHA-256：`ec150bd2126f41a7b48ab0e368ef983ac80b5d54afad28065c43e03ddeee756e`。本地归档镜像摘要相同，发行清单已更新。

本版包含短信转发启动配置尚未确认时 fail-closed 的 Rust 修复，以及 CoreAudio IOProc 分阶段退休修复。CoreAudio 现在只在 `AudioDeviceStop` 与 `AudioDeviceDestroyIOProcID` 都确认成功后释放回调上下文；失败会保留尚未退休的注册，下一次启动先重试清理，设备路由恢复也不会越过未确认的旧 IOProc。正常停止和注册失败的错误会反馈到通话页面。

验证结果：`scripts/test-call-audio-store.sh` 的 29 个通话路由场景和 IOProc ledger 的3组状态机检查通过；Rust 格式检查与 `scripts/test-release-atomic.sh` 通过。Rust daemon 的完整回归采用 Iteration 598 的结果：3031项在首轮通过，另外5项因命令遗漏隔离变量 `SWITCH_DATA_DIR` 而失败；补齐变量后原5项逐一通过。原完整命令退出码为1，不能描述为一次性全绿。

3.45 Release 构建成功。DMG 的 `hdiutil verify`、只读挂载、版本/构建号、正式 Bundle ID、arm64 主程序与后端、deep/strict 签名核验均通过。包内后端为 `switchd-rs`，带有必要的 `libusb-1.0.0.dylib`，没有 Go 后端。签名为本机 ad-hoc，未提交 Apple 公证。

本轮未替换已安装应用，也未操作读卡器、SIM、短信或拨号；CoreAudio 状态机回归使用模拟音频/API，不能替代真实通话音频验收。SIM World 被动收信按用户要求暂缓。P0未闭合，P1–P4验收中，P5语音完善中，P6最终验收未收口。接口接管仍为294/294；整体验收分母未冻结，不报告整体百分比。

源码变更及测试边界见[Iteration 599](optimization/all-rust/iteration-599-coreaudio-retirement.md)和[整体进度](optimization/all-rust/progress-current.md)。
