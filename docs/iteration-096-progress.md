# Iteration 096：模块射频策略与恢复迁入 Rust

日期：2026-10-05。全 Rust 后端目标仍在执行。本轮只在 Switch 内修改和验证，没有启动生产 App、访问生产短信或 Keychain、操作实体卡、拨号、发送短信或生成 DMG。

## 接管范围

Rust 成为 `module-flight-mode.json` 唯一写入者，并接管公开 GET/POST `/api/module/flight-mode`、启动射频屏障、3 秒默认策略、业务预约、VoWiFi 射频占用与恢复，以及 USB/串口上的完整 CFUN 操作计划。Go 原偏好写入、策略定时器和逐命令射频控制退出生产；Go 仍保留设备配置、通话/eSIM/VoWiFi 调用者及兼容适配，不能据此宣称全部后端已迁移。

七个私有 POST 动作位于 `/api/internal/module-radio/`：configure、query、bootstrap、refresh、reservation、vowifi、action。它们要求内部凭证、当前 producer 和严格 DTO。公开请求体限 4 KiB、内部限 32 KiB，读取期限 5 秒；服务最多受理 8 项作业，串行等待限 30 秒，同一作业的观察、设置、核验与恢复共享 55 秒物理预算。Go 内部等待 95 秒、仅释放预约为 65 秒；Swift 精确 GET/POST flight-mode 为 100 秒。实际驱动返回及耐久文件写入仍须等待真实完成，不能把这些期限当作强制释放硬件的保证。

## 兼容与恢复

- 保留默认关闭射频、用户明确开启蜂窝、手动/默认/外部/VoWiFi 所有权、原状态字段及九类公共错误码。故障详情采用固定脱敏信息；不完整回复后的操作结果仍明确标为未知，不自动重放。86 组冻结的旧 Go 原始响应通过真正的 Rust 解析器和状态投影比较，错误码另由实际失败分支验证。
- 偏好文件采用原对象格式、0600 临时文件、原子替换及文件/目录同步；坏文件拒绝覆盖。稳定 CFUN=4 的定时检查不重复保存偏好。原子抑制状态直接约束 Rust 串口和 USB 查询，避免旧 Go 缓存继续发出蜂窝探测；解除抑制后兼容采集器恢复状态刷新。SIM 身份及 AKA 不受蜂窝探测门禁误阻。
- 长期 VoWiFi intent 与短时 AT/SIM 事务分开，避免占用整个鉴权期间的物理通道。释放和恢复只作用于原 token/sequence/会话，先到的清理会阻止迟到请求复活。eSIM 切换先确认停止 VoWiFi，再取得业务预约；竞争失败时不开始卡操作。
- 原始 CFUN 写入口被关闭，合法手动诊断通过统一授权路径完整发送原命令；保留大小写、引号及复合 AT 命令的既有处理。物理计划确认 CFUN、检查通话状态，并在同一会话内设置、轮询和恢复；不重开另一设备补写。已提交但无完整终态时关闭旧流，迟到 OK 不能被当作下一条命令的确认。
- 活动预约最多保存 64 个 operation ID 的结果，相同请求返回原结果，不因丢失 ACK 再次发命令。未知状态在预约释放后仍保留，新 token 不能绕过；只有实际只读确认后才能继续新写入。Go 子进程退出会撤销 RF 专用代次许可，等待已提交 I/O 结束，但禁止旧计划继续发下一步命令。
- 启动屏障遇到未绑定、业务占用或未知结果时明确失败；后台 Tick 可以跳过。暂时失败后，同一设备后续确认成功可幂等补建 eSIM 管理器，不必重启，也不在屏障失败时提前触卡。
- 通话事件只更新有界协调器，不在 URC 回调中等待长 RPC。拨号预约与 pending 状态同锁建立；RING→CLIP 不丢失已有通话意图；丢失释放确认后先使用原 token 完成清理。退出等待真实预约释放，不能用超时伪称完成。

## 验证证据

最终 `scripts/test-migration-gates.sh` **exit 0**：Rust workspace **1096 通过、0 失败、13 个跨语言夹具默认忽略**；其中 daemon **613 通过、0 失败、2 个默认忽略**。85 组 USB 状态与 86 组 RF 状态/九类错误差分在专用步骤实际执行通过，不能将默认忽略当作未验证。Go vet/全部兼容测试、运营商参数回放和 SDP/AMR 差分也通过。历史 PCMA helper 仅是离线夹具，不是生产语音选项。

Go 三包完整 race 为 **765 个顶层 / 1357 个含子用例通过、2 个既有可选夹具跳过**；这份全包结果早于末端恢复修正。随后最终定向 race 为 **107 / 311 通过、0 失败、0 跳过**，覆盖拨号窗口、同会话启动恢复、CLIP 更新及未确认 Bootstrap ACK。两次重叠运行不相加成独立用例总数。Swift 请求策略 **11 组通过**，包含实际 URLSession 的 flight-mode GET/POST 100 秒、鉴权与单次提交，以及真实 Unix 桥期限。

契约写入和一致性检查 exit 0，16 项工具自测通过；当前 220 条 method/path（Rust 122、Go 98）、127 个 Swift 调用点、162 项审阅 scope。Rust-only 检查按预期 exit 1：默认 Go fallback 和 144 条 Go 所有权/依赖仍在，不能报告 Go 退出。

最终日志与汇总位于 `Switch/build/optimization/`：

- `2026-10-05-radio-owner-migration-gates-final.log`
- `2026-10-05-radio-go-final-race.jsonl`
- `2026-10-05-radio-go-final-cutover-race.jsonl`
- `2026-10-05-radio-swift-request-policy-final.log`
- `2026-10-05-radio-owner-ownership-final.log`
- `2026-10-05-radio-contract-results.json`（含四项契约命令结果与日志路径）
- `2026-10-05-radio-owner-results.json`（本轮完整汇总）

初次测试中遗漏的集成 fixture 字段和新增 ownership 组件计数断言已修复，原失败日志保留。后续 Rust 全量和独立差分通过；最终另格式化 Cargo 不遍历的 `include!` 测试文件，未改变测试语义；公开路由归属表达式等价拆分以适配现有契约提取器后，10 项 ownership 定向测试全部通过。Rust workspace/独立测试格式、脚本语法及 `git diff --check` 通过。仍有两个既有 native VoWiFi dead-code warning；未做本轮完整 App 构建或硬件验证。公开接口目前只验收 GET/POST；旧 Go GET 注册隐式支持的 HEAD 仍会落入 Go fallback，尚未等价迁移，连同 405/Allow 语义列入 P0 统一 HTTP 兼容验收，发布前必须补齐。

## 进度与后续

仅本轮 RF 策略和执行接管计入 P2B：**44% → 47%**，增加 **0.240 计划人周等值**。总计 **11.825 / 47.5 ≈ 24.9% 已完成、75.1% 待完成**，估算误差约 ±3 个百分点。P2A 保持 61%，P3 不变，P5 保持 0%，不重复计分。新增独立 RF ownership 组件使运行时组件清单从 16 项变为 17 项，Go-free 仍为 6 项；其显示值从 37% 变为 35% 只是清单分母变化，不是上述项目加权进度。

语音目标继续为 **EVS 优先、AMR 回退，不使用 PCMA**。当前主机能力仍是 AMR-NB mode 7，EVS/AMR-WB 尚未实现；EVS 新增平台和 codec 成本仍待试点重估，不提前纳入已完成分值。9eSIM `6982` 永久排除。

下轮先补公开 HTTP 方法兼容边界并建立永久回归，再继续设备/卡片会话、模块与读卡器剩余业务、原生 IMS/VoWiFi/N待、完整语音及最终 Go 退出。真实模块 CFUN/CLCC 差异、插拔重枚举、长时间恢复及运营商验收仍缺，离线通过不能替代这些门禁。契约与权责细节见 [RF 接管契约](optimization/all-rust/radio-owner-contract.md)和[实施仪表](optimization/2026-10-04-all-rust-execution.md)。

后续进展：Iteration097已补齐上面记录的规范路径HEAD/旧405方法缺口，详见[Iteration097](iteration-097-progress.md)；URL清理、转义段与尾斜线仍需验收。本页保留Iteration096当时的结果。
