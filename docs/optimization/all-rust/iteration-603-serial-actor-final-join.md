# Iteration 603 — 串口 actor 的最终关闭确认与路径校正

日期：2026-10-21。所有源码、构建、证据和运行数据均在 `/Users/StatTrak/Desktop/Switch_MAC/`。没有修改原始参考目录，也没有生成 DMG。

## 实现

串口 actor 的普通 `Close` 保持可重开语义；新增终态 `Finalize`：actor 完成会话关闭、执行最终清理并回执后，控制端再等待并 join actor 所有者线程。最终关闭回执缓存为稳定结果，重复 shutdown 不会把“收到 close 回执”误当成 actor 线程已退出。相关 admission 分支已更新，现有 API 合同/路由语义不变。

项目中的活动执行路径已统一为 `/Users/StatTrak/Desktop/Switch_MAC/`：

- 源码：`/Users/StatTrak/Desktop/Switch_MAC/src/`
- 构建、运行数据和证据：`/Users/StatTrak/Desktop/Switch_MAC/build/`
- 应用和发行物：`/Users/StatTrak/Desktop/Switch_MAC/dist/`

同步修正了 `Switch_MAC/docs/Switch改造方案.md`、`Switch_MAC/docs/Codex实施任务书.md` 和 `Switch_MAC/docs/implementation/实施与验证记录.md` 的当前执行路径。旧 `/Users/proletarius/Desktop/Switch/` 只作为初始要求或历史记录保留。Codex 内部目标记录仍含旧路径；当前可用 goal 工具只提供读取和状态更新，不提供 objective 文本编辑，因此不能伪称内部记录也已修改。

## 验证

| 检查 | 结果 |
| --- | --- |
| `proletariusd` 全量测试（正确载入 `scripts/workspace-env.sh`） | 3036 通过、0 失败、8 忽略；运行 1257.18 秒 |
| `native_cli_tests` / `native_process_tests` / `native_recovery_tests` | 2 / 5 / 3 通过 |
| Rust workspace（排除已单独测过的 `proletariusd`） | 67 个测试二进制，849 通过、0 失败、1 忽略 |
| Rust `fmt --check` | 通过 |
| Rust API 合同盘点 | 294 method/path；294 Rust owner、0 Go owner；129 Swift 调用点；470 review scopes；54 个 HEAD alias 不额外计数 |
| Go 后端退出结构门 | 通过；无 Go 构建、打包、运行、分发或源码导出边 |
| BLE reader operation proof | 通过；Unix bridge、回执、冲突、超时、关闭和重启证明均为无硬件测试 |
| BLE manager | 39 个生产事件序列组通过；含 9eSIM NBLE/B1 认证向量，仍是模拟测试 |
| 冻结协议夹具 | 6466 个直接案例完整性检查；297 次协议回放通过；没有执行 Go |
| EVS 构建 | 370 个对象生成 `libswitch_evs.a` |
| 验收账本 | 元数据一致，294 API / 131 Swift / 56 RT；115 个需求叶、21 类场景；未冻结总分母、没有新增功能 PASS |
| Release `.app` 候选 | `Switch.app` 3.46/build 346，arm64，deep/strict 签名有效；仅包含 Swift 主程序与 Rust `switchd-rs` 两个可执行文件 |

全量 daemon 测试首次有一次命令漏载环境变量，失败日志保留在 `build/reports/iteration-603-daemon-full.log`，不计验收；正确环境的重跑日志为 `build/reports/iteration-603-daemon-correct-env.log`。完整 workspace 日志为 `build/reports/iteration-603-other-rust-workspace.log`。

## 验收边界与剩余工作

本轮 USB 树枚举出 ESTKme-RED，但候选应用的隔离 UI 显示读卡器未连接、VoWiFi 未连接；没有读到 SIM 身份/Profile，也没有实际 IMS 注册。因此这只能证明 USB 设备枚举，不能记为 SIM World 卡片或运营商验收。没有读取入站短信，没有发短信或拨号。P1–P4 实机/运营商验收未完成，P5 语音与 P6 最终验收仍未收口。

UI 核验中按显示名选择应用曾短暂启动旧的 candidate581（3.43）及其后台；随后通过精确 bundle ID 正常退出，并确认两个 Switch 测试实例的前后台进程均已结束。未发短信、未拨号；没有查看或迁移用户数据。后续以 `.app` 绝对路径核对本轮 3.46 候选。

清理脚本删除了 8 个白名单可重建缓存目录：`build/rust-target`、DerivedData 中间/模块/SDK缓存，以及 Python/Swift cache。清理前脚本统计 19,790,278,656 allocated bytes；`df` 可用空间从约 640 GiB 增至 651 GiB。日志、依赖、运行数据、Release Build Products、`dist/Switch.app`、源码与恢复材料保留。清理后 `scripts/clean-generated.rb --list` 没有剩余候选缓存。

整体验收分母仍未冻结，整体百分比不报告。294/294 仅代表静态 API 所有权覆盖，不代表产品完成。现有 3.46 DMG 未在本轮重制；本轮只生成、核验了 `.app` 候选。
