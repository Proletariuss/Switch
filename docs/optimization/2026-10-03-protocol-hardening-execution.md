# Switch 2.80：协议加固与可用性实施记录

本轮目标见 [项目目标](2026-10-03-protocol-hardening-goal.md)。2026-10-03 开始，基线 2.79/build279，交付版本 2.80/build280。适用软件验收、Release 与最终 DMG 核验已完成。条件性候选及实机未验收项按下文明确停止或暂缓，不计作功能已实现。

## 基线与范围

实际源码冻结：3299 文件，51,739,679 字节，归档 SHA-256 `29d304bad6199c982f373e6e61d955946068af4a06c99dc51298c7c17c2f8845`，位于 `build/optimization/2026-10-03-protocol-hardening-baseline/`。原有 dirty/untracked 均属于基线，不能用 Git HEAD 统计本轮修改。

工作只发生在 Switch 内；没有启动生产应用、访问运营商、切换 Profile、发送真实短信、读取生产短信正文或更改原始参考目录。9eSIM 6982 及下载认证文件永久排除。保留现有 SwiftUI / Rust 网关与协议库 / Go 硬件及 IMS 运行时，保留通话和 N待 SMS-only。

## H1：外层用户态 ESP 正确性 — 实施

改前夹具实际证明：重复、过旧和零序号可被接收；并发相同包可多次接受；发送 counter 转成 uint32 后回绕；GCM 的 `append(salt, iv...)` 可覆盖共享 key material 中相邻密钥。

实施边界：

- 每 SA 维护 64 包窗口；初检不修改窗口，通过认证、ESP padding 与 IP 长度检查后，锁内重新检查并提交。合法乱序保留，失败认证不能推进窗口，并发重复只能通过一次。
- 发送序号以原子 CAS 饱和到上限，后续返回耗尽错误，不产生序号 0 或重新使用旧序号。SWu netstack/TUN 发送循环因此终止，取消子会话，由既有 `Connect → watchSWU → handleRuntimeFailure` 生命周期处理恢复；没有新写 rekey 策略。
- GCM nonce 使用单独存储，不写入借用密钥的剩余容量；内层 IMS 保护、加密协商及算法不变。
- 检查 ESP 编号 padding、IPv4/IPv6 声明长度，允许合法 IP 分片并移除 TFC padding；测试包同步改为合法 IP。
- 最终 race 门禁发现共享日志对象懒初始化与读取竞争，补 RWMutex 保护 Logger/Sugar 的发布、替换与稳定快照，保留原 Init/注入语义；独立旧版负例可复现。数据通道夹具等待 Tx/Rx 双退出，避免跨测试残留。相关四包 race 连续 20 次通过，未关闭 race 检查或靠延时掩盖。

当前 Switch 使用 SWu netstack 路径。独立 `ipsec.DataPlane` 库发送循环只获得拒绝回绕保护，其独立生命周期不在本轮适配内。耗尽在本地日志中明确记录，跨层恢复原因仍归入现有 `tunnel_lost`。本轮没有声称修复 swu-go 所有历史并发回调，也没有修改 3GPP/运营商协议。

证据：`build/optimization/2026-10-03-outer-esp-hardening/` 中改前失败和修复后 race 日志；最终验收日志另存 `2026-10-03-protocol-hardening-final/`。

## H2：短信持久记录故障隔离 — 实施

原先两份存储启动 recover 的错误直接终止网关，诊断也不可用。现在设备发件账本与 N待存储各自锁存故障，网关及 Go 子进程可继续启动。故障 store 的正式 API 保持禁止提交；健康另一份存储继续可用。

- 损坏、文件权限/路径问题、容量超限、写入/rename 后同步失败及 worker 故障不当作空库成功，不清空原记录。
- 存储锁序列化检查、磁盘操作和故障公布；故障之后排队的 claim 也不能放行。旧无 ID 发送、私有 prepare/claim/finish、N待队列和入站确认均纳入边界；未保存的入站消息不确认成功。
- 本进程观察过的文件消失、已有空文件/null/[]拒绝自动重建。同进程修好文件也不解锁；新进程 recover 重新验证。prepared/accepted 保留，submitting 恢复为 unknown，不重新授予发送许可。
- N待停止、占用释放、历史运行报告仍可用；受损来源的导出必须明确失败，不能把损坏结果输出为“0 条成功”。独立审查发现的“首次访问为导出”路径另补回归并修复。
- `GET /api/v2/runtime` 增加白名单 `sms_store_health` schema 1。网关 `ok:true` 不等于短信 store 健康。对应 API 返回 `503 sms_store_unavailable` 和不可自动重试语义；诊断只返回固定原因代码，不包含路径、号码、正文。
- Swift 诊断页独立显示设备发件与 N待存储状态，仅在打开、后端状态变化或手动查询时更新。缺少字段、未知/不一致 schema、失败及取消后的旧响应均不能显示“正常”；没有新增轮询。

范围限制：设备发件账本不是设备收件归档，不能把此能力表述成所有存储故障都完全隔离；高级手动 AT 继续遵循原策略，账本不是任意厂商命令沙箱。外部删除整个账本后冷启动、历史合法文件回滚没有独立防回滚证明；本轮不新增数据库或防篡改体系。

证据：`build/optimization/2026-10-03-sms-store-isolation/` 保留启动改前失败、故障注入、真实 Unix HTTP 夹具与 daemon 回归。全部夹具使用隔离文件和模拟消息，不是实卡记录。

## H3–H7：条件性候选与停止决定

| 候选 | 决定 | 依据与重新开启条件 |
| --- | --- | --- |
| 同会话 IMS REGISTER 续期 | 本轮停止生产接入 | 当前成功注册未保存完整 Call-ID/tag/CSeq/nonce 上下文；运行响应按 Call-ID 分派，复用注册事务需专用 method/CSeq/代际匹配。不是改一个 timer 即可；估计 5–10 人日，加运营商验证。缺少 ≥3 次自然周期和 24h 对照，无法证明重建次数和不可用时间各下降 ≥50%。保留现行 planned_renewal。 |
| 真实业务闭环 | 本轮未验收，明确保留缺口 | 两次只读检查未见运行的 Switch 三进程；已询问当前卡片/Profile，尚未确认本轮设备。没有把历史 PAC 授权当作当前设备选择；没有为得到数据自动连接卡片。真实收发、语音、并发与长期恢复不能用模拟结果替代。 |
| 后台资源/轮询调度 | 停止参数修改 | 无当前可见/隐藏/最小化各 3×10min 公平基线。沿用现成 PID 身份/CPU 时间/RSS 采样工具，进程缺失记为不可测。没有证据达到 CPU ≥20% 且 ≥0.5pp 收益及短信时延门槛，因此不调慢轮询、不制造性能百分比。 |
| 架构文档 | 实施 | 建立当前目标入口，更新 README/ARCHITECTURE；历史迁移评审标明失效，纠正 Send/Sync/async 锁自动避免死锁或业务原子性的错误推论。 |
| IMS / VoWiFi / IKE / IPsec 全量 Rust 接管 | 不启动 | 已有 Rust 协议库并不等于运行接管完备；最终会话 owner、rekey/重认证、SMS-only 业务与共享实体卡 AKA broker 仍需端到端验收。缺少瓶颈、成本回收和实卡对等证据。保留已经复用的库和桥接，不新增逐包 HTTP/JSON IPC，不删 Go。 |

同会话续期审查全文：`build/optimization/2026-10-03-protocol-hardening-review/c9-renewal-feasibility.md`。资源可测性记录：同目录 `runtime-availability.json`，`cpu/rss/wakeups` 为 null，`real_ims_verified:false`；不能解释成 0 消耗或测试成功。

重新启动条件性优化须先形成上述证据，再按原成本门槛决策；本轮不留下默认启用的实验运行路径、闲置注册器或重复采样器。

## H8：验收与最终交付

三轮独立审阅已完成，导出绕过守卫和首个 worker panic 返回状态问题已修复并复核。初轮 Rust workspace 518 项通过；导出修复后 daemon 209 项通过。Go 主模块与 swu 相关包 vet 通过，Rust fmt 检查通过。

第一次统一并发门禁在 swu TUN 退出夹具中发现全局日志懒初始化的竞争，门禁失败即停止，未打包。`functional-gate.log` 保留失败；补齐同步与测试退出等待后，四包 race 连续 20 次和独立静态复核通过。

最终软件验收：

- `sh scripts/test-functional-optimization.sh` 完整成功，日志 `functional-gate-final.log`。包含 Go 短信/IMS/设备/事件及 swu/ipsec/crypto/logger race、Rust 存储/网关/导出相关测试、16 组 Swift 脚本及 14 项真实 Go/Rust 跨进程检查。所有硬件、运营商与通知 provider 均为隔离夹具。
- 初轮 Rust workspace **518 项**成功；后续新增导出/worker边界后，受影响的完整 daemon **209 项**成功（11 组健康隔离、16 组导出、21 组 N待存储等含在其中）。这两个计数有重叠，不求和当作独立测试数量；后续改动只涉及该 daemon，其他 Rust crate 未继续修改。
- 最终 Rust fmt 成功；Go 主模块及 swu/logger/ipsec/crypto vet 成功。独立审查覆盖 ESP/密钥、短信故障隔离、前后端 DTO/UI生命周期及日志初始化修复，无未解决发现。
- `sh scripts/build.sh` Release 成功。`app-verification.json` 核对 **2.80/build280**、`com.proletarius.switch`、最低 macOS 27.0、四个 Mach-O 均为 arm64、Go 的 libusb 使用 `@loader_path`、嵌套与外层 strict 签名、图标和许可证一致。签名为本机 ad-hoc，未公证；没有运行生产应用。
- 构建仍有既有 SDK API 废弃提示、重复 libusb 链接提示与 Rust未读取错误字段等警告；本轮不为清除警告而改下载认证或音频行为，没有声称零警告。日志保留于 `release-build.log`。

最终交付：

- 文件：`/Users/proletarius/Desktop/Switch/dist/Switch-2.80-arm64.dmg`，46,900,367 字节（约 46.9 MB）。
- SHA-256：`3c0e21f397b2c418201082e0b19d26b00744b39c15b4cf558254ff604318064c`。
- `hdiutil verify` 成功；只读挂载核验 bundle 内 **26 个文件**的 SHA-256 与权限均匹配已验证的 `dist/Switch.app`；版本/构建号正确、Applications 链接正确、deep strict 签名通过，随后卸载。没有启动安装包中的生产应用。明细 `dmg-verification.json`。
- 源码范围审计：26 个修改、11 个新增，0 删除、0 权限变化；143 个 eSIM/eUICC/download 排除文件内容与模式不变。Go mod/sum 全部不变，Cargo 外部依赖完整区块不变，11 个本地 crate 只升版本。最终重算见 `source-scope-audit.json`。
- 收尾源码冻结位置为 `build/optimization/2026-10-03-protocol-hardening-final-source/`，其 manifest 是构建输入和文档的可复核快照；不要用 dirty Git HEAD 替代。

本轮只在最终验收后生成上述一个 DMG，未生成中间版本，也未覆盖旧 2.79 DMG。合格候选已实施；续期策略、后台资源参数与全量 Rust 接管因证据/成本门槛未满足而停止。真实运营商短信、语音、长期在线和资源收益仍未验收。
