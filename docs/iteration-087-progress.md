# Iteration 087：语音编码目标更新与主机 AMR 停用

> **历史记录，策略已被覆盖。** 用户随后明确更正为 **EVS 优先、AMR 次选，不使用 PCMA**。下文保留 Iteration 087 当时实施及测试事实，不代表当前目标；目前主机过渡能力为 AMR-NB mode 7，EVS/AMR-WB 尚未实现。最新范围见 [EVS/AMR 计划](optimization/2026-10-05-evs-pcma-plan.md)。

日期：2026-10-05。依据用户新要求：EVS 首选、PCMA 次选、不再使用 AMR。所有修改和测试产物位于 Switch；Celmux 目录只读。未启动生产 App、拨打真实电话或操作卡片，未生成 DMG。源码变更需要后续构建运行后才影响已安装应用。

## 已实施的过渡策略

- Go 主机 VoWiFi 的出呼 SDP 只宣告 PCMA 与 telephone-event，移除 AMR-NB 协商、codec 状态和上下行实际编码分支。当前无 EVS encoder/decoder/音频桥，故没有提前宣告 EVS。
- 来呼及远端 answer 不接受 AMR、AMR-WB 或 EVS-only。保留静态/动态 PCMA、payload type 检查和 PCMA 早期媒体后的空 SDP 最终应答。缓存 codec 也不能绕过媒体启动检查。
- 入呼无共同 codec 使用已有 488 拒绝路径。PRACK/UPDATE 对显式 application/sdp 正文增加隔离的 codec 检查，不支持时返回 488；PCMA、空正文及非 SDP 保留既有响应。验证不会改写 live codec、RTP endpoint 或 DTMF 状态。
- Rust SDP 与 Go 同步为 PCMA-only；删除 AMR enum/选择与专用 fmtp helper。Go↔Rust 的 AMR 历史样例改成明确拒绝，增加 EVS-only 与非法 PT8 重映射拒绝。
- 通话页面显示“PCMA · AMR 已停用”，说明 EVS 尚未接入，不再显示“AMR 兼容”。

既有通话中的 re-INVITE 仍返回 Busy，UPDATE/PRACK 没有实现媒体重新协商；本次只补编码拒绝门禁，不能据此计为完整 offer/answer 状态机。AMR framing/FFI/静态库构建引用暂保留，但不再用于主机实时通话；历史 framing 测试门禁名称同步标注为非生产能力。后续无引用清理仍在目标中。

## 评估、复用与兼容取舍

主机目标正式改为 **EVS Primary → PCMA**，同时禁止 EVS 内 AMR-WB IO 回退。完整计划及来源见 [EVS/PCMA 评估](optimization/2026-10-05-evs-pcma-plan.md)，主方案 P5 已同步。

Celmux 的 MIT EVS RTP/SDP/CMR 边界可作后续复用基础；native 桥及外部参考 C codec 有各自来源条件，现有构建不支持 Darwin。本轮没有复制 EVS 源码、下载 codec 或声称可用。Swift 8 kHz PCM、16 kHz EVS RTP 时钟、DTMF、丢包补偿及实际双向音频都需单独改造与验收。

交叉审阅补充两项约束：Primary 初始模式不能保证通话全程无 IO；收到有效 IO 要求时必须合法重新协商 PCMA 或受控结束，避免持续无声。EVS 扩大 P5 工作量，旧分母进度不能当作新目标精确完成率。

原记录中部分 CTExcel/EE 和 O2/giffgaff 通话依赖 AMR，停用后可能无法建立通话。PCMA 并非所有运营商都接受；本次按用户取舍执行，不宣称提高所有运营商兼容性。模块作为 SIM 的主机 VoWiFi 适用此策略；模块原生蜂窝语音由 DSP/固件协商，当前没有已验证控制接口，不能宣称其 AMR 也已禁用。短信与 N待行为未因本轮 codec 策略改变。

## 验证

- `proletarius-call` 离线单测 **29 项通过**；SDP fixture 构建成功。
- Go imscore 全包 `-race` **144 个顶层测试通过**，含子用例为 **270 项**；**27 个 Go↔Rust SDP 差分案例实际执行通过**。新增门禁包含16种有/无旧PCMA的拒绝组合、5种缓存绕过、PT8非法映射及16种PRACK/UPDATE响应与状态不变案例。
- 全包首次运行因未指定独立 codec fixture 跳过 PCMA/AMR 两个旧 parity；随后构建两种 fixture 并以 `-race` 单独重跑，**两项均通过**。AMR 仅验证保留的历史 framing，不代表允许协商或实际通话使用。
- 上层 Go macOS 后端 `-race -run 'Call|Voice'` 通过；仅有既有重复 libusb linker warning。
- Swift `CallView.swift` 语法解析、workspace rustfmt、gofmt 和 diff 检查通过。未进行完整 App 构建或界面运行验收。
- 最终接口契约扫描通过：196项契约（Rust94、Go102）、127个Swift调用点、76处审阅指纹；本次未改变路由归属。日志：`2026-10-05-evs-policy-contract-check.log`。

证据位于 `Switch/build/optimization/`：`2026-10-05-voice-no-amr-imscore-race.jsonl`、`2026-10-05-evs-policy-codec-parity.log`、`2026-10-05-evs-policy-api-tests.log`、`2026-10-05-evs-policy-fmt.log`、`2026-10-05-evs-policy-swift-parse.log`。Rust单测结果保留于本次工具执行记录，未为写日志重复运行已经通过的测试。

## 进度

原 **47.5 人周迁移基线进度仍为 22.2%**，误差约 ±3 个百分点。此次策略变化没有完成 Rust 生产媒体 owner 或 EVS，因此不增加迁移分数。EVS 新增成本待 macOS codec 试点后重估 P5 与总分母；未将旧22.2%无条件称为扩大范围后的总完成率。主目标继续推进，9eSIM 6982 仍排除。
