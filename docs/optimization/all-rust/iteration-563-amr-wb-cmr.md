# Iteration 563：AMR-WB 运行中 CMR 与连续编码状态

2026-10-18。补齐原计划 P5 要求的 AMR-WB 运行中码率请求：当前 SDP 不再把允许模式集合丢弃为一个最大值，真实媒体 worker 在合法 RTP 请求后的下一编码帧边界调整同一个原生编码器。EVS 优先、AMR-WB 兼容的顺序保持；没有加入 AMR-NB 或 PCMA，也未触碰真实卡片、拨号、短信或生成 DMG。

## 行为与约束

依据 [RFC 4867 §4.3.1、§8.2、§8.3.1](https://www.rfc-editor.org/rfc/rfc4867.html)：OA/BE 解析器保留 CMR 高四位；0–8 只有位于协商集合才有效，15 取消先前待完成请求并保持当前发送模式，保留值和集合外请求不覆盖已知请求。模式改变遵守 period=1/2 的固定帧相位，并按 active set 中的相邻模式逐步切换。

`AmrWbModePolicy` 保存允许集合与对端对本地发送的 period/neighbor 约束。主叫支持全部模式与子集，因此 offer 省略 mode-set，并声明 mode-change-capability=2；被叫 answer 保留收到的有效 mode-set，不把对端的接收约束镜像成新的发送要求。初始仍沿用最高允许模式；合法上下行语音模式必须在集合内，SID/NO_DATA/丢帧保持既有处理。未实现或未接受的封装参数继续拒绝。

CMR 只能在匹配来源、经 reorder/sequence 检查且 payload 成功解码之后进入当前通话 worker。原生 C setter 只更新下一帧码率和必需配置，保留 predictor、decoder 与 `last_*` 历史；媒体使用编码器返回的实际模式生成 FT，不替换 RTP socket/SSRC/sequence/timestamp。无效/旧代操作不能更改正在使用的 worker。顺带修正坏质量 AMR-WB 帧送 PLC 时携带非空 bits 导致拒绝的问题；该帧内容不用于语音解码。

## 实际离线验证

使用 Rust 1.98.0、locked/offline、明确的 `../build/rust-target`。本轮测试禁用 debug symbol 和 incremental 缓存以降低磁盘占用；不影响生产 Release 配置。重建 370-object EVS archive 后执行真实 codec 测试，未用缺失 codec 时跳过代替成功。

| 证据 | 结果 |
| --- | --- |
| call crate：全部 RTP/SDP/原生 codec 与独立向量 | 47/47 通过；`build/reports/iteration-563-call-tests.log` |
| daemon 媒体：OA/BE、连续调码率、period/neighbor、15、保留值/集合外、畸形、错误源、重复/迟到、旧 binding、退出 | 6/6 通过；`iteration-563-media-tests.log` |
| 原有 call API 与 call owner 定向回归 | 20/20、43/43 通过；`iteration-563-call-api.log`、`iteration-563-call-owner.log` |
| 完整 production public call fixture | 1/1 通过；`iteration-563-production-call.log` |
| 原封存独立 AMR-WB/SDP 样本 | 24＋57 通过，原 fixture 未改；`iteration-563-frozen-call-fixtures.json` |

完整 production fixture 通过公开通话控制建立原生 IKE/IMS 会话与真实 codec/RTP/ESP/PCM 通路，在同一个已接听的 AMR-WB dialog 中连续 34 帧测试 8→2→8、CMR15、9–14 全部保留值及全部集合外语音模式。先等待载有 CMR 的下行音频实际解码并回到 host，再发送下一 PCM 帧；逐帧验证上行 FT/长度、相同 SSRC、sequence+1、timestamp+320 和零解码错误。它是本地合成运营商对端，不证明真实运营商音质。

独立 High reviewer 审核源码与测试同步屏障，未发现新的 P0/P1。契约审计 294 Rust/0 Go、127 Swift HTTP、54 HEAD、431 review scopes 保持；actual/recorded 的路由、owner、consumer、依赖、Swift 调用及 scopes 完全相同，仅更新来源行号，不改旧 source-review SHA 或历史样本。`iteration-563-contract-reviewed.log` 记录显式刷新后通过。Q=false PLC 负例加入后重新编译，媒体最终 6/6 仍通过：`iteration-563-media-tests-final.log`。

## 完整构建与包内验证

`scripts/build.sh` 已完整构建 `../dist/Switch.app`（3.43，开发产物未换发行号），严格嵌套签名校验通过。包内 `switchd-rs` SHA-256 为 `ba3a25f9c6c6a07933ea2a9fa1beb5f1d8f7517c928c90c35a5a35f4423d3a57`，后端目录仍只有它与 libusb，没有 Go。旧实卡受测 559 产物另保存在 `build/release-3.43-iteration559/Switch.app`，没有覆盖已安装应用或生成 DMG。

`iteration-563-build-source-snapshot.json` 冻结 1443 个输入。构建后核对只发生一项 cfg(test) 文件变化（补 Q=false 回归），生产输入无漂移；另存 test-source manifest 和 `iteration-563-build-source-audit.json`，最终媒体测试已包含它。完整构建日志为 `iteration-563-full-build.log`，Rust 格式与 Go 退出检查通过。

实际包内 daemon 在 546 mandatory 硬件/外连沙箱中通过 pending+pristine 共存启动验证：模块 reason 包含 `legacy_execution_pending`、历史恢复 false、模块操作提交前 409、reader 投影与合成归档仍可用、pending inode/bytes/mtime/ctime 不变、正常退出 0 且无 socket 残留；私有运行目录已删除。报告与可复跑 runner 为 `iteration-562-pending-bundled.json` / `.py`。

同一包再次执行旧3.42→新版→旧版→新版的同目录回退夹具：`iteration-563-inplace-bundle-roundtrip.json` 中数据往返、新版隔离和两次新版正常退出均通过；旧版本仍退出1，因此 overall `passed=false`、`full_p6_acceptance=false`，不吞掉旧失败。

最终产物信息写入 `../dist/development-build-manifest.json`，明确是当前开发包，未替换已安装应用、未生成DMG。确认无Cargo/Xcode进程或相关打开文件后，已删除本轮重新生成的 `build/rust-target` 与 `build/DerivedData`；保留离线依赖、签名应用、旧基线与报告。磁盘最终可用约651 GiB。

## 保留的验收边界

本轮未重新验证 SIM World 实网；[557–560](iteration-557-simworld-nat-notify-registration.md) 是上一个具体二进制的实卡注册证据。当前真实呼入/呼出、双向语音及音质、EVS 运营商支持、短信、正常 LPA、N待多 Profile、自然租期长稳和 P6 旧回退仍需各自证据。仅关闭 AMR-WB CMR 的软件实现缺口，不能据此标记 P5、P6 或整体 100%。
