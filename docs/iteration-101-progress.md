# Iteration 101 — 来电布局、设备短信转发来源与 PC/SC 断开交接

日期：2026-10-07。全 Rust 后端改造继续按 `docs/optimization/2026-10-04-all-rust-backend-plan.md` 执行。本轮只在 Switch 工作区内修改和验证；未进行实卡操作、拨号或发短信，未生成 DMG。9eSIM `6982` 仍不在范围内。

## 已完成的可验证变更

- 来电卡片里的“接听 / 挂断”按钮由自适应网格改为两侧等宽 Spacer 的水平排列，使按钮组相对通话卡片居中；活动通话的按键与挂断也沿用同一布局。删除通话编码标签中重复的 EVS 分支。
- Rust 设备短信转发生产者不再经 Go `/api/sms` 拉取列表，而是直接使用 Rust 公共短信列表相同的归档 + 有界易失快照合并投影。Go 物理短信采集仍在过渡期保留。Go 私有快照附带当前子进程 producer ID；Rust 拒绝旧代/未知代快照，并在已确认 Go 退出后优先轮换 ID。Go 启动快照在私有监听器或归档暂不可用时限速重试；此时转发保持未准入。
- 内部转发的交接导入、baseline/disabled 账本更新和队列 + 账本同步提交，均受同一 producer 栅栏保护。旧代退出与本地耐久提交按同一 InboxState 互斥锁线性化，避免检查后等待期间旧代批次跨代落盘。
- 生产 Go 定时热插拔监测改为只负责 BLE；PC/SC 实体缺席由 Rust 按两次成功扫描确认，再锁住精确 reader/session/producer/sequence 并关闭物理句柄。关闭真正完成后，Rust 通过私有回调交给 Go 处理旧业务代次；Go 等 VoWiFi/IMS、N待 runtime 与 owner reservation 实际退出才 ACK。回调失败或超时，Rust 保留该 reader 的准入栅栏并重试；旧事件无法清理新会话。BLE 采样绑定读卡器名与 generation，不能误清切换后的 PC/SC 选择。

## 验证与基线

- Rust 短信归档定向测试 20 项、转发生产者 14 项、服务提交栅栏 1 项通过；Go `go test ./cmd/proletarius-macos -count=1` 通过。命令和日志见 `Switch/build/optimization/p2a-device-sms-verification-2026-10-07.txt`。`cargo +1.98.0 fmt --manifest-path rust/Cargo.toml --all -- --check`、`git diff --check` 通过。
- Rust `reader_pcsc::tests` 30/30 通过，相关 Go 热插拔/代次交接定向 race 测试通过。合并两组改动后，Rust daemon 全包 **670 passed、3 ignored、0 failed**；Go `go test ./cmd/proletarius-macos -count=1` 再次通过，日志分别为 `Switch/build/optimization/iteration-101-daemon-tests.log` 和 `iteration-101-go-tests.log`。本项仅是离线 fake-I/O 与并发验证，没有实体读卡器拔插结果。
- `scripts/test-workspace-presentation.sh` 通过。布局检查是源码/隔离呈现测试；本轮尚未收到真实来电截图回归证据。
- `scripts/build.sh` 的完整 Release 构建和代码签名验证通过，产物为 `Switch/dist/Switch.app`，日志为 `Switch/build/optimization/iteration-101-release-build.log`。未替换当前已安装 App，也未生成 DMG。
- P0 契约清单已显式刷新并通过一致性检查：249 条 method/path（Rust HTTP owner 150、Go owner 99，含 20 条 HEAD 派生别名）、127 个 Swift HTTP 调用点、191 个指纹化审阅范围。41 项检查器反例自测通过。新增的 Go 私有 PC/SC 失踪回调是第 99 条 Go owner，不代表迁移比例倒退；它在过渡期负责旧业务退出确认。`--require-rust-only` 按预期拒绝默认 Go fallback 与仍含 Go 归属/依赖的 160 条路由；这是退出门禁仍有效，并非验收通过。
- 对已安装的历史混合后端 Switch 2.86/build 286 做了一段连续 600 秒的只读固定 PID 采样，覆盖 App、Rust 网关、Go 子进程。20 个有效 CPU 区间中，合计 CPU 平均 7.3232%（单逻辑核口径），合计 RSS 平均 159702 KiB，采样期间进程身份连续。原始记录、环境和汇总位于 `Switch/build/optimization/2026-10-07-p0-live-baseline/`。这是**一段未分类空闲窗口**，未控制前后台、代理/Profile 数量，也未验证 IMS 在线；不能用来判断迁移收益或代替计划中的前后台各 3×10 分钟基线。

## 尚未完成

- 若 PC/SC 实际拔卡时物理 `close` 返回错误，Rust 会保留占用与准入栅栏，不把关闭错误当成功释放；此时旧 Go 业务和界面状态可能延迟撤销。是否会在具体读卡器上发生仍需实机拔插测试。进一步的“先撤销旧业务、但保持实体栅栏”双阶段事件尚未实施。
- 设备短信物理采集、IMS/VoWiFi/N待/语音、LPA 和其余 Go 业务仍在迁移；Go 子进程、默认 fallback 和 Go 构建依赖尚未退出。P0 完整性能/DTO/硬件基线与最终实机、长稳、Go-free 验收均未完成，进度不标 100%。

原 47.5 人周口径的加权估算暂维持 **11.825 / 47.5 ≈ 24.9%（约 ±3 个百分点）**；本轮是来源与退出安全边界的局部接管，没有把尚依赖 Go 的实体短信、VoWiFi 或语音链计为完成。EVS 新增范围使总分母仍待重新估算。
