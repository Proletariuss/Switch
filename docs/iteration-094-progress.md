# Iteration 094：USB 完整设备状态采集迁入 Rust

日期：2026-10-05。全 Rust 后端目标继续执行，尚未完成。所有变更、缓存和测试数据均在 Switch；没有启动生产 App、访问实际设备、读取生产短信、操作号卡或生成 DMG。语音保持 **EVS 优先、AMR 次选，不使用 PCMA**；实际主机能力仍为 AMR-NB mode 7，EVS/AMR-WB 尚未实现，本轮不增加语音得分。

## 接管范围与收益

在 Iteration 093 的唯一 USB AT owner 上，Rust 新增完整设备状态采集，而非只增加一个字段解析 helper。新私有认证 `POST /api/internal/usb-at/device-status` 要求当前 producer、session、SIM lease 和蜂窝探测抑制标志，在同一真实句柄/事务内执行 CFUN、固件、SIM、信号、注册、运营商、网络制式、ICCID、IMSI、号码、IMEI 与 USB 网络模式查询，返回 15 个类型化字段。

正常计划 14 步；偏好要求 RF-off 或 CFUN 确认处于飞行模式时跳过 6 个蜂窝步骤，只执行 8 步并生成原有飞行模式展示。单步沿用 3 秒期限，CFUN 为 4 秒，每步响应最多 1 MiB，整个 native 计划 55 秒；Go 观察上限 75 秒，公共 GET 的原 30 秒期限不变。Go 不再逐条发送或解析该状态计划，保留 PLMN 显示名、已存号码回填、页面缓存及 RF 策略。

稳定已打开会话的公共状态读取经测试只产生 **4 次内部 RPC**：status、acquire、device-status、release；不再为每个 AT 字段单独请求和获取/释放占用。这是离线请求计数与归属改善，尚无实体 USB 延迟/CPU/长稳收益数据。首次打开、清理未确认占用和忙重试不承诺相同次数。

## 生命周期和兼容边界

- 采集不自动打开、重绑或重试设备；每一步前后及最后发布前检查原 session、lease、producer 代次和 VoWiFi inhibit。开始 VoWiFi 时先在 native owner 设置 inhibit，避免 Go 状态登记与真实硬件准入脱节。
- 请求取消只取消等待；真实阻塞 I/O 完成前继续占有设备。任何传输/归属错误停止整个状态计划，真实传输失败关闭旧句柄并进入既有 2 秒冷却；不把多个会话的部分字段拼接成成功状态。AT 文本 `ERROR` 则沿用旧可选字段默认值，不等同传输断线。
- Go 在原 native lease 仍持有且本地生命周期读锁有效时发布确认缓存。若随后 release 未确认，向调用者返回错误并保留清理 token，已确认的历史观察仍可留在缓存；这不是 release 成功或设备永久在线的证明。
- 交叉审阅发现并修正 Go 状态读取持有 SIM 锁后再取 radio lease 锁、而 VoWiFi 恢复按相反顺序取锁的死锁风险。完整 VoWiFi 状态快照放在 SIM/native 事务外，事务内仅检查短期 ownership 元数据。
- 生命周期锁等待、owner status 和 open 继承调用方 context；取消排队观察不会随后偷偷打开或查询设备。独立清理期限保留，以等待真实 I/O 收尾。

兼容投影由旧 Go 实现独立生成 85 组输入/预期结果，再由 Rust collector 读取同一 fixture，逐项比较所有字段与实际命令顺序。覆盖正常/RF-off、AT 失败文本、空值、数值边界、IMEI/ICCID/IMSI/号码、网络制式及注册响应。Go 正则 ASCII 字符类和数值宽度/溢出行为保持；旧 `firstNonZeroRegistration` 只匹配 `+CEREG`/`+REG` 而不匹配 `+CREG` 的行为也保留，未在迁移时顺便改产品语义。首次手写 Rust 用例误以为 `+CREG` 会回退，出现 1 项失败；已校正测试预期并通过独立 Go 导出核对，初始失败日志保留。

## 验证记录

- `scripts/test-migration-gates.sh` 离线依赖运行 **exit 0**。Rust workspace **1011 项通过、0 失败、12 项 fixture 忽略**；其中 daemon **528 项通过、0 失败、1 项 fixture 忽略**。新 Go→Rust 差分在后续专用 gate 配置 fixture 后实际运行 **1 项通过，覆盖 85 组案例**，不能把普通 workspace 的忽略项直接当通过。
- 新 collector 普通单测 9 项通过；USB owner 总计 26 项（本轮新增 7 项）通过，覆盖固定计划、RF-off、严格 lease/停机边界、取消后的真实占用、最后一步 inhibit、旧 producer 和断线实际 Drop。HTTP 测试增加新路径的内部认证、公开端口隔离、错/缺字段、方法及关闭拒绝。
- Go 全主包 race **486 个顶层 / 876 个含子用例通过，0 失败**，2 个既有可选 fixture 跳过。最终定向 race **13 / 72 通过，无失败或跳过**，覆盖必填字段/类型、错误 session、VoWiFi、RF 默认与坏偏好文件、无重放、旧结果隔离、缓存发布锁边界及取消准入。
- Go vet 与全兼容测试、2722 个运营商回放及 SDP/AMR 差分通过。历史 PCMA helper 仅作离线历史兼容验证，未启用为生产编码。没有运行本轮 Swift/App 构建或实机验收；两个既有 native VoWiFi dead-code warning 仍存在。

日志均在 `Switch/build/optimization/`：`2026-10-05-usb-status-{daemon-tests,collector-tests,parity-tests,migration-gates}.log`、`2026-10-05-usb-device-status-go-{focused,cmd-race}.jsonl`，以及 85 组 `2026-10-05-usb-device-status-legacy-parity.json`。永久门禁每次从旧 Go 测试投影重新导出 fixture，再执行 Rust 显式差分；临时数据在 Switch 内并在退出时清理。

接口清单写入及一致性检查 exit 0：**211 条 method/path，Rust 111、Go 100，127 个 Swift 调用点，139 项指纹审阅范围**。本轮新增一个 Rust 私有动作及两个完整文件 scope，复核五个既有变化 scope。16 项契约工具自测通过；Rust-only 检查按预期 **exit 1**，拒绝默认 Go fallback 和 136 条 Go 归属/依赖路由，这不是零 Go 验收通过。最终日志前缀为 `2026-10-05-usb-device-status-contract-`，`results.json` 保存确切退出码。Go manager/imscore 历史 raw string 的静态词法范围限制继续由人工审查与 race 补足。

Rust workspace 格式及独立 include 测试按项目 edition 2024 检查通过；`git diff --check`、门禁 shell 语法和本轮文档 131 项本地链接检查通过。最终 Go/Rust 交叉审查未发现新增阻断项；独立进度复核确认 P2B 增量及加权总和正确，顺便更新实施记录中遗留的旧接口总数，保留历史迭代原有统计。

## 进度与后续

本轮仅计入完整 USB 状态计划生产接管及离线证据：P2B **41% → 42%**，增加 **0.080 计划人周**。总计 **11.425 / 47.5 ≈ 24.1%**（估计误差约 ±3 个百分点）；P2A 保持 61%，P5 保持 0%。原 47.5 人周口径不含尚待试点重估的新增 EVS 平台/codec 成本，不能解释为全部扩展范围的精确完成率。

RF 策略、偏好单写者、启动/3 秒执行器、VoWiFi 临时 radio lease 和最终恢复仍由 Go 控制，不能把 RF-off 展示迁移当作完整 RF owner 接管。下一步须覆盖业务准入、恢复确认、定时器实际退出与串口多命令预留；USB 手动存储删除路径另有选择确认、旧索引复核和精确历史清理风险，已在[后续边界审计](optimization/all-rust/modem-next-cutover-review.md)登记，本轮不宣称修复。

实体 USB 插拔/驱动阻塞/重枚举、真实号卡、运营商差分与长稳仍待验收；eSIM/AKA/IMS/N待/语音及 Go 最终退出继续推进。9eSIM `6982` 永久排除，不生成中间 DMG。
