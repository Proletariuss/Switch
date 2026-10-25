# Switch 3.46（构建 346）

2026-10-20，Apple Silicon，macOS 27.0 及以上。本机 ad-hoc 签名，未公证。

[下载 DMG](/Users/StatTrak/Desktop/Switch_MAC/dist/Switch-3.46-macOS27.dmg)。大小 49,928,257 bytes；SHA-256：`74bbc725e2c83050d7c80d6516aaeaf128164bfcbff823e5f0b6c14a23fa059b`。本地归档副本与发行清单已生成，摘要一致。

本版从 Switch_MAC 当前源码重新构建 Release app、Rust daemon 与 EVS 组件，并将版本升级到 3.46/build 346。Rust daemon 的 SHA-256 为 `41517e41f221eba05643282ec413c628026fe78bd8d336dcb56c816ad264dab6`。

验证结果：Xcode Release 构建成功；DMG `hdiutil verify` 通过；只读挂载后确认版本 3.46、构建 346、arm64 主程序与 `switchd-rs`。包内 backend 目录只有 `switchd-rs` 和 `libusb-1.0.0.dylib`，没有 Go daemon；app deep/strict 签名验证通过。发行脚本隔离流程测试通过。验收台账校验器报告 metadata consistent、294 条 API、131 个 Swift 调用点、56 条 runtime 能力记录、115 个需求叶子和 21 个场景类别；其42项自测通过。Go 后端退出结构检查通过。

严格 Rust 路由门禁仍未通过：`sms_forwarding.rs` 的整文件审阅指纹要求人工复核，不能把已声明的294/294 Rust API owner 当成该门禁已通过。P0仍未闭合，P1–P4验收中，P5语音完善中，P6最终验收未收口；整体验收分母未冻结，因此不报告总体百分比。接口归属覆盖单列为294/294。

本轮没有安装应用、操作读卡器/SIM、发送短信或拨打电话；设备矩阵、运营商业务与长稳验收仍须单独完成。签名为本机 ad-hoc，未提交 Apple 公证。
