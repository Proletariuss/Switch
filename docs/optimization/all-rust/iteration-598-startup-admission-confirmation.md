# 工作包598：启动未确认时保护短信转发游标

本轮修复启动期间的配置判定：Rust daemon 刚启动而 SwiftUI 尚未恢复 Keychain 中的转发设置时，`active=None` 只能表示“尚未确认”，不能等同于用户明确关闭。`Service::admission_enabled` 在没有已完成的准入模式写入时返回 `sms_forwarding_configuration_unconfirmed`。只有实际配置请求写入并确认关闭模式后才返回 `false`；既有明确关闭期间推进高水位、重新启用不回填的行为不变。

N待生产者收到未确认结果时在读取页面及推进 durable cursor 前返回错误。设备短信生产者在读取 inbox、创建通知 baseline 或提交消息之前返回错误。用户已暂缓实机入站短信，本轮只做本地隔离软件测试。HTTP接口和DTO未变化。

## 验证

`cargo fmt --all --manifest-path Cargo.toml -- --check`通过。完整 daemon 测试二进制实际运行20分48.66秒：3031通过、5失败、8忽略。初次命令传入`SWITCH_BUILD_DIR`但遗漏仓库要求的`SWITCH_DATA_DIR`；5项失败均在读取隔离运行目录时panic。完整原始日志保留在[测试日志](../../../../build/reports/iteration-598-daemon-full.log)，SHA-256为`a7241db57dacbf260eb638dfbc2a6eb34733f28d565ef87c346c870bd7be4aec`，不可将该次进程记作全绿。

在隔离目录`build/runtime/iteration-598-data-root`补齐`SWITCH_DATA_DIR`后，5个原失败用例逐一以精确名称重跑，全部通过，结果见[补跑报告](../../../../build/reports/iteration-598-failed-tests-correct-env.json)及[补跑日志](../../../../build/reports/iteration-598-failed-tests-correct-env.log)。因此3036个非忽略用例名均有通过证据，另8个保持忽略；这是完整运行加5项环境纠正补跑的合并证据，不是一次全绿完整运行。首次单项运行还发现关闭 Service 后旧测试期待`Ok(false)`；当前没有新配置确认时实际应保持 unconfirmed，修正断言后已包含在完整运行并通过。

## 当前边界与下一步

没有读卡器、号卡、短信、通话或DMG操作。接口接管仍为294/294；P0未闭合、P1–P4验收中、P5完善中、P6未完成。整体N及百分比未冻结，不作推算。下一个已定位的生产缺口是 CoreAudio `AudioDeviceStop`/`AudioDeviceDestroyIOProcID` 失败时仍释放回调上下文，以及部分启动失败后无法可靠重试销毁；应在实际系统确认退休前保留上下文并阻止重启。

本轮生产源 SHA-256：

- `sms_forwarding.rs`: `f5050ac0ed988b09be8c4e5eaaf97ebc009b1818e869ce35f063df3b995a47bd`
- `forwarding_admission.rs`: `664b13dfa5de55e7587b30376977ead9957065d5f8f8636807d024b7143f4f0e`
- `device_sms_forwarding.rs`: `39f5faa774aeb3e47a334fb455267e1c3ed5e6d6804f0e96081572c4fe529888`
- `sms_forwarding/retirement_tests.rs`: `2eab115bce0fcf6d44e8fa2ebb41c0a86fb923c189d55e7e94c13418176bf1a7`

完整结构化核对见[最终报告](../../../../build/reports/iteration-598-final-review.json)。
