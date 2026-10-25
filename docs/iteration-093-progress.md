# Iteration 093：USB AT 句柄与事务由 Rust 持有

日期：2026-10-05。全 Rust 后端迁移继续进行。本轮所有源码、缓存、测试数据和日志位于 Switch；没有启动生产 App、访问实际 USB 模块或读卡器、操作号卡、收发真实短信或生成 DMG。语音目标保持 **EVS 优先、AMR 次选，不使用 PCMA**；当前主机实际能力仍为 AMR-NB mode 7，EVS/AMR-WB 未接入。本轮不增加语音完成度。

## 实际迁移范围

Rust 新增 `usb_at_transport` 和 `usb_at::Owner`，持有唯一 USB AT libusb 句柄，负责设备筛选/探测、普通命令与 prompt 数据提交、会话代次、跨 RPC 的 SIM 事务占用、语音独占关闭、短信采集禁入与本地 SM/ME 原始快照。Go 的真实 AT libusb 实现移除，改为携带 producer、session 和事务 token 的私有 RPC 适配器；缺少 Rust owner 时明确失败，不回到 Go 打开硬件。Go 的 ADB 传输仍存在，不能把 AT 迁移称为所有 libusb/语音已去 Go。

Rust 自动收件直接调用同一 owner，移除 Go 生产 `POST /api/internal/usb-sms/snapshot`；旧实现仅保留在离线测试中作为兼容参照。短信自动采集与 eSIM、AKA、发送/删除等多步事务因此使用同一硬件准入。eSIM 从连接、打开逻辑通道到 CCHC/断开保留占用；固定发送路由不因旧句柄失效而选择新会话或重放短信。Go 仍负责设备/射频策略、业务路由、eSIM/AKA/IMS/语音流程与配置持久化，未达到完整零 Go。

设备选择保持 DJI `2ca3:4006` 优先、Quectel `2c7c:0125` 回退，只探测接口 2/3、class/subclass/protocol `ff/00/00` 且同时具有 bulk IN/OUT 的候选；避开 diagnostics、NMEA 和 QMI 接口。设备描述使用实际 VID/PID；常规发现先读取 Rust 已打开会话状态，避免原 ioreg 仅识别 DJI 而底层可打开 Quectel 的不一致。session ID 只表示句柄代次，不冒充 SIM/Profile 身份。

## 请求、取消与恢复边界

新增七项认证私有 POST：`/api/internal/usb-at/{open,status,acquire,release,close,command,configure}`。严格字段校验、1 MiB 请求/响应上限、非零有限 I/O 期限和有界输入排空避免无限等待/增长；SM/ME 各最多 240 KiB，必须有完整 OK。已读取的成功存储可在另一存储断线时保留，但不会继续使用失效句柄。普通命令的部分响应兼容原有语义，不把它升级为短信采集成功。

- `producer_id` 随 Go 子进程换代；确认旧进程退出后撤销其 lease、配置和旧 session，等待实际 I/O 与句柄释放。请求取消只取消等待，不释放还在执行的硬件操作。
- `lease_seq` 高水位阻止 release 先到、acquire 后到时复活旧事务；旧 close 不能关闭其他 token 正在持有的事务。竞争 acquire 立即返回 busy，避免等待者占满内部 UDS 连接、使持有者无法发送 command/release。Go 只对明确的 acquire busy 有界重试；硬件 command 单次提交。
- 短写、prompt 后超时和未知 ACK 保留结果未确认，不补写或重放短信；prompt 失败只作一次有限 ESC 收尾。失败的 release 保留原 token，后续先确认清理，不能直接丢弃占用身份。
- 语音切换先取得 Rust voice lease，撤销旧 session 并等待真实句柄 Drop 后确认，再允许 Go ADB helper 改变 USB composition。固定旧路由保持失效，正常发现才可采用新会话。
- VoWiFi 启动至活动、退休和确认射频恢复期间保持短信采集 inhibit。`inhibit_seq` 阻止已取消的暂停请求迟到复活；`config_seq` 阻止旧短信接管配置覆盖新设置。相同序号仅接受完全相同的重试；进程换代清空高水位。
- 退出先停 Rust 收件 worker，再等 Go 通信收尾；期间仅允许状态、占用释放/关闭以及严格 CFUN 查询/恢复命令，拒绝短信/APDU等新操作，最后关闭 Rust USB owner。驱动未实际返回不能视作已退出，实机阻塞行为仍须单独验收。

交叉审阅还覆盖 eSIM 打开逻辑通道失败的完整事务释放、原生采集断线后 Go 缓存 session 的更新，以及迟到的旧错误不能关闭重连后的新 session。相关修复与最终验证结果见下方。

## 验证记录

Rust daemon 最新 **512 项通过、0 失败、0 忽略**，包含 19 项 owner、16 项 transport 和原生收件/归档整合测试。新增确定性测试覆盖取消仍占用实际 I/O、真实 Drop 后 voice ACK、producer 换代、旧 lease/close、配置乱序、SM 成功/ME 断线及固定旧会话拒绝；均使用假传输，不连接真实设备。主 HTTP 测试覆盖内部认证、公开端口隔离、停机允许项与禁止项。

Go 首轮全主包 race **471 个顶层、800 个含子用例通过，0 失败**；2 个既有可选 fixture 因环境未配置跳过，不计作通过。交叉修复后的定向 race **81 个顶层、155 个含子用例通过，无失败或跳过**，覆盖打开逻辑通道失败释放、旧错误不关闭新会话及旧USB发送入口复用完整固定事务。

日志前缀：`Switch/build/optimization/2026-10-05-usb-owner-*` 与 `2026-10-05-usb-at-*`。完整 `scripts/test-migration-gates.sh` 以离线依赖执行并 exit 0：Rust workspace **995 项通过、0 失败、11 项既有跨语言 fixture 忽略**；Go vet/全部兼容测试、2,722 个运营商案例及 SDP/AMR 差分通过。历史 PCMA helper 差分也通过，仅为离线历史辅助验证，未重新加入生产协商或媒体链路。日志为 `2026-10-05-usb-owner-migration-gates.log`。未运行本轮 Swift/App 构建或实机验证，不增加此类通过声明。


接口清单写入和一致性检查通过：**210 条 method/path，Rust 110、Go 100，127 个 Swift 调用点，137 项指纹审阅范围**；移除 1 项 Go 原始快照接口并新增 7 项 Rust 私有动作。16 项契约工具自测通过；Rust-only 检查按预期 exit 1，拒绝默认 Go fallback 及 135 条 Go 归属/依赖路由，不把预期拒绝称为零 Go 验收通过。删去 3 项已删除源码的旧 scope，新增 9 项，既有 scope 逐项复核。Go manager/imscore 的历史 raw string 词法限制仍由人工审查及 race 补足，不冒充自动覆盖任意行为。日志为 `2026-10-05-usb-at-contract-{write,check,selftests,rust-only}.log`。

Rust 格式（含独立 include 测试）、`git diff --check`及本轮文档本地链接检查通过。最终 Go/Rust 交叉复核未发现新增阻断项。

## 进度与后续

本轮只将USB AT真实owner接线、完整事务边界及离线验证计入P2B：**37%→41%**，增加0.320计划人周；总计 **11.345 / 47.5 ≈ 23.9%**，估计误差约±3个百分点。P2A保持61%，P5不增加。EVS原生编解码/macOS平台工作的新增成本仍需试点重估，23.9%沿用原计划分母，不代表扩展后全部范围的精确完成率。

运行时组件登记因新增独立的USB AT职责条目，从15项变为16项；其中6项为已独立的Rust职责、9项仍阻止Go退出，另有保留的平台职责；不能把Rust拥有入口等同于无Go依赖。组件粗略百分比由40变为37是分母细分的结果，与上述人周加权项目进度不同，不能混用。

剩余范围仍包括设备状态与射频策略、eSIM/AKA/通话业务、ADB与读卡器/IMS/N待流程的Rust接管；USB实体插拔、阻塞驱动、重枚举、实际号卡、运营商差分和长稳尚未验收。下一步继续迁移这些业务与生命周期，复用本轮唯一USB owner及事务机制；不将模拟测试当作实机完成。9eSIM 6982继续排除，无DMG。
