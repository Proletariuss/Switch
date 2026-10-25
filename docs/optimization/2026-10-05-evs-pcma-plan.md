# EVS / AMR 语音评估与实施计划

日期：2026-10-05。**用户最新指令：EVS 首选、AMR 次选，不使用 PCMA。** 该指令覆盖此前“EVS Primary→PCMA、禁用 AMR”的临时政策；Iteration 087 仅保留为历史实现记录。为避免现有链接失效，本文件继续使用原路径 `2026-10-05-evs-pcma-plan.md`，标题和当前实施要求以本版为准。

全 Rust 后端主目标不变，N待继续只处理短信。所有实施与构建位于 Switch，Celmux 参考目录只读，中间不生成 DMG。

## 结论与取舍

主机 VoWiFi 的目标编码顺序为 **EVS → AMR**，不协商 PCMA。当前真正可使用的 AMR 回退是 **AMR-NB mode 7，8 kHz、12.2 kbit/s**；EVS 和 AMR-WB 均未接入，不能因有协议结构或编译 feature 就提前宣告。是否建立通话仍取决于本地 encoder/decoder 可创建、运营商/对端共同能力及实际媒体链路。

EVS 内的 AMR-WB IO 不再被政策禁止；但允许并不等于已经实现。只有 IO 编解码、协商、CMR/帧模式切换及媒体格式通过验证后才能启用，不能把 AMR-NB 或一个失败的 Primary decoder 当作可用 IO 能力。EVS Primary 与 IO 的实际模式需要明确记录，不能将所有 EVS 载荷都标成 Primary。[TS 26.445 Annex A](https://www.etsi.org/deliver/etsi_ts/126400_126499/126445/18.00.00_60/ts_126445v180000p.pdf)

恢复 AMR-NB 能保持现有 AMR-only 入呼的协商可能性；停用 PCMA 后，PCMA-only 通话会因无共同编码被拒绝。本次未进行运营商实卡通话测试，不宣称所有 O2/giffgaff、CTExcel/EE 或其他线路均已可用。AMR-WB 缺失仍是现状，不把“EVS→AMR”目标当作完整 MTSI codec 兼容证据。[TS 26.114 §5.2.1](https://www.etsi.org/deliver/etsi_ts/126100_126199/126114/18.11.00_60/ts_126114v181100p.pdf)

## 实际基线与本次范围

| 路径 | 核对结果 | 调整方式 |
| --- | --- | --- |
| Go 生产 IMS `call_runtime.go` | 原实现已有 AMR-NB mode 7 与 PCMA；无 EVS/AMR-WB | 恢复经过能力检查的 AMR-NB 编解码、协商和媒体分支，停用主机 PCMA；不虚报 EVS |
| Rust `proletarius-call` | 已有 AMR framing、PCMA/RTP/SDP/DTMF helpers；未接管生产通话 | SDP 与 Go 同步为当前 AMR-NB mode 7 过渡规则；完整媒体 owner 留在 P5 |
| Swift `ReaderVoWiFiAudioBridge` | 固定 8 kHz / mono / s16 / 20 ms / 160 samples | 当前匹配 AMR-NB；EVS 上线前扩展显式媒体格式，不能把旧 PCM 直接交给宽带 codec |
| 模块作为 SIM 的 VoWiFi | 模块提供 APDU/AKA，Mac 处理 IMS/RTP | 使用相同主机 EVS→AMR 目标与当前 AMR-NB 过渡策略 |
| 模块原生 VoLTE/蜂窝语音 | DSP/固件处理编码，Mac 接 UAC；没有已验证 codec 优先级控制入口 | 独立发现实际能力；不能宣称已经禁用模块 DSP 的 PCMA |

“全 Rust 后端”要求业务、协议、设备调度及恢复归 Rust，不要求重写成熟编解码算法。经来源核验并有安全封装的 AMR/EVS 原生 FFI 可以保留；不能把 Go 静态库包装成 FFI 作为零 Go 完成。现有 AMR framing/库和向量门禁继续服务生产迁移，取消旧计划中按停用 AMR 而删除它们的要求。PCMA 历史工具和夹具不等于允许生产协商，可在确认没有必要引用后独立清理，不按关键词删除模块诊断字段。

## Celmux 可复用范围

只读来源：`/Users/proletarius/Downloads/celmux-src`。本轮没有复制 EVS 源码、下载 codec 或执行参考构建脚本。

| 参考部分 | 实现与许可核对 | 使用决定 |
| --- | --- | --- |
| `crates/media/evs/src/payload.rs`、`fmtp.rs`、`control.rs` | 文件标 MIT；有 RTP Compact/Header-Full、ToC、CMR、SDP 参数 | 优先复用协议边界解析，保留归属声明；Primary 与 IO 能力分别验证 |
| `crates/media/evs-native/src/lib.rs` / `shim.c` | Rust wrapper 标 PolyForm Noncommercial；shim 无单独 MIT 标记，调用实际 C encoder/decoder | 不能作为全 MIT 引入；记录来源条件，保留窄 FFI 与明确内存/线程/关闭责任 |
| `scripts/build-evs-native.sh` | 另取 3GPP TS 26.443 C 参考实现并校验 SHA-256；当前目录不含算法资产 | 真正算法并非纯 Rust；先核验版本、来源、许可和分发条件 |
| 原 native 构建 | 现有脚本针对 Linux/Windows GNU/musl，无 Darwin 分支 | 需要 macOS clang/Mach-O 静态归档与符号隔离；按 Switch 现有 arm64 平台验收 |

不能照搬的逻辑包括按 feature 宣告 EVS、native 初始化失败后隐式切换到未验证的 IO、只解码 RTP 首帧，以及把 MD5 语料校验当作算法 conformance。允许 IO 后，失败回退仍必须满足已协商能力、双向 codec 可用性和当前媒体格式，不能吞掉 Primary 初始化错误而伪装为正常 EVS。

EVS 使用 16 kHz RTP 时钟，20 ms 为 320 ticks；PCM 采样率与 RTP 时钟需要独立配置。`evs-mode-switch=0` 只约束初始/更新时模式，后续有效 CMR 与网关变化可能要求 IO；需要完整状态处理，不能将它当作全程 Primary 的保证。[TS 26.445 Annex A](https://www.etsi.org/deliver/etsi_ts/126400_126499/126445/18.00.00_60/ts_126445v180000p.pdf)

## 实施顺序与完成门槛

1. **建立 AMR-NB 过渡策略。** 出呼仅宣告实际可用的 AMR-NB mode 7 与 telephone-event；answer 限定 PT97、mode-set 仅为7、octet-align=1，禁止扩大本地 offer 的模式或切换到未宣告 framing。入呼保留动态 PT96..127、mode-set 包含7及原有 OA/BE 兼容边界。无共同编码使用明确拒绝，不自动回退到 PCMA 或重复拨号。UI 说明当前 AMR-NB、EVS 待接入。
2. **完成 EVS 原生能力验证。** macOS arm64 encoder/decoder 双向创建、帧边界、失败/关闭、静音/语音向量与独立参考解码验证；编译成功不足以宣告可用。Primary、IO、AMR-WB 的真实能力分别登记，不把名字相近视为等价实现。
3. **扩展媒体契约。** 明确 PCM rate/channels/format、frame duration、RTP clock、DTMF clock 与会话代次。保留 AMR-NB 的8 kHz回归；EVS先验证宽带PCM，再按实际码率/带宽能力扩展。采集、播放、重采样、丢包补偿和队列上限必须一致。
4. **接入 Rust 实时链路。** SDP→codec→RTP→Swift PCM 由同一媒体 owner 控制。在 EVS 双向实际可用时按 EVS、AMR 排序；音频帧不经过逐帧 JSON HTTP。覆盖多帧载荷、动态 PT、丢包/乱序、SSRC变化、DTX/PLC、时钟及 DTMF。
5. **完成模式和通话状态验收。** 来去电、183/PRACK/200、CANCEL/200竞争、re-INVITE、切设备、静音、挂断及记录。IO按验证后的能力允许；有效CMR/帧切换必须驱动一致的encoder/decoder状态，不能忽略后持续无声。无法支持的模式通过合法offer/answer退到共同AMR能力；没有共同编码或重新协商失败则受控结束，不暗中启用PCMA。
6. **最后收口与实机。** 保留成熟 AMR FFI，完成 Rust会话owner对codec句柄、线程及关闭的管理，移除 Go编排依赖。用明确测试号码和线路确认双方可听并分别记录运营商支持；模块DSP路径另列实际codec及可控性证据。无实机证据不标记完整语音完成。

## 测量与成本控制

- 正确性优先：双向向量、协商与媒体匹配、AMR-NB的OA/BE回归、EVS Primary/IO模式测试、PCMA/PCMU拒绝及无共同编码错误。不能用性能提升抵消这些失败。
- 性能基线改用相同机器/帧时长的当前 AMR-NB 链路；测每20ms帧编码/解码p50/p95/p99、实时队列积压、掉帧、进程CPU/RSS和端到端延迟。EVS使用相同样本与网络故障轨迹复测。
- 初始准入预算：预热后双向codec处理p99不超过10ms/20ms帧，连续30分钟不出现计算积压导致的无界排队；这是项目工程门槛。采集/播放和网络抖动分别计量，不把平均CPU低当作没有爆音。
- 优先采用经验证的 codec FFI，不从零重写 AMR/EVS 算法。只有实测超过预算才做性能改写；单项预计收益不足10%或无法稳定复现时停止微优化并记录取舍。
- 原47.5人周是全Rust迁移基线；增加EVS平台/codec、宽带媒体与模式切换需先完成试点后再校准P5和总分母。最新原基线进度见[执行看板](2026-10-04-all-rust-execution.md)；本次政策更正不增加迁移得分，包括EVS新增工作的总目标百分比待重估。Iteration 088 的22.5%来自短信编排迁移，并非EVS已经完成。

当前已确认的 Rust 协议门禁为 `proletarius-call` 26项离线测试通过，SDP fixture 构建成功；Go策略与差分验收由本轮实施记录另行登记。UI语法检查不等于完整App构建或界面运行。Iteration 087的旧PCMA-only测试事实保留但不作为本策略验收；本计划不表示EVS已接入或任何运营商已经通过实卡通话。
