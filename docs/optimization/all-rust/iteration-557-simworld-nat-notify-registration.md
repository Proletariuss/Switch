# Iteration 557：SIM World 两次 IMS 注册通过

2026-10-18。主审使用同一个已复审 Release 候选进行了两次独立、仅注册的 SIM World 454/03 实卡探测；两次均达到 **IMS `registered`**。本说明只读汇总已完成的报告，没有新增实卡、代理、安装或 DMG 操作。

受测 daemon 来自 [Iteration 556 隔离候选](iteration-556-module-recovery-quarantine.md)，SHA256 均为 `e66efe25bd0768c16041c5517cfec3e8098b9f5e79f7ea486b3bf4359d2b09a9`。构建/签名及原始 bundle 指纹在 `build/reports/iteration-556-candidate-bundle.json`，不是另一次未经核对的重建。

两轮沿用既定 Surge 自动代理配置，固定此前旧版成功的 ePDG `124.217.186.49`；报告均确认 `pinned_selection_address_equal=true`、`surge_unchanged=true`。执行 harness 要求有效 Surge 代理归属、系统代理和 Enhanced Mode 开启，没有改为直连或另做本地 DNS。该控制保持配置/目标一致，不等于已证明两次外部网络状态完全相同。

| 对照 | 观察结果 |
| --- | --- |
| [552](iteration-552-simworld-286-rust-registration-ab.md)：旧 2.86 / Rust | 旧版 IMS registered；Rust 及固定旧版成功 ePDG 的补测均失败，ESP 8 发 / 0 收 / 0 拒绝 |
| [553](iteration-553-ike-device-identity-parity.md)：新增可选 DEVICE_IDENTITY(41101) 的 Release | 注册仍失败，ESP 8/0/0；单补 41101 未解决该现象 |
| [554](iteration-554-simworld-natt-port-diagnostic.md)：隔离的 pre-41101 端口诊断副本 | 最终已验证 AUTH 来源和首个 ESP 目标均为 4500，仍失败 8/0/0；未合入端口跟随改动 |
| 557：本候选首次实测 | IMS registered；disable、reader disconnect、daemon 退出、私有 runtime 删除全部确认成功 |
| 558：同一候选重复实测 | IMS registered；上述四项清理再次全部成功 |

相对 553 已包含 41101 的版本，本候选唯一新增的 **IKE 协议变化**是在 SA_INIT 发送 NAT_DETECTION_SOURCE_IP / NAT_DETECTION_DESTINATION_IP 通知，并在 COOKIE 重试中保留它们；其他增量为 556 模块业务隔离。没有借本轮修改 AUTH/ESP 算法、4500 选择、端口跟随或代理策略。

该增量与两次真实注册成功呈**强相关**。离线字节/COOKIE 单测支持实现符合本次设计，但不是运营商因果证明；尚无撤回 NAT 通知后的反向实测或真实解密握手对照，不能据此断言唯一、绝对根因。两份成功摘要的 `ports`、`counters` 均为空，不将其解释为零流量，也不补造 ESP 收包数。

证据为工作区 `build/reports/simworld-557-nat-notify-live.json` 与 `build/reports/simworld-558-nat-notify-repeat.json`；两者 `registered=true` 由最终 `ims_state == registered` 生成，四项清理均 true、`error_code=null`。没有发送短信、拨打电话或切换/下载 profile。通话、短信及长期稳定性仍未测；[556](iteration-556-module-recovery-quarantine.md) 的旧 3.42 回退退出失败、历史模块恢复与未包装降级 marker 证明边界仍未闭合，**不能据两次注册将 P6 / 全量迁移验收标为完成**。

## 完整源码构建的复核

随后执行 `scripts/build.sh`，Xcode 和 Rust Release 构建成功，`codesign --verify --deep --strict` 通过；产物为 `../dist/Switch.app`（版本 3.43），包内后端仅 `switchd-rs` 与 `libusb`，没有 Go 可执行文件。完整构建日志在 `build/reports/iteration-557-full-build.log`。因正式构建重新编译，包内 daemon 的 SHA-256 为 `ee31ec8f617a791413453e71d9e32857a59aca9dae05d92b452746c206a4a775`，与前述隔离候选的哈希不同，不能直接借用其结果。

因此对这个**实际完整构建产物**再做一次相同的 SIM World 454/03 注册探测：`build/reports/simworld-559-full-build-live.json` 记录 `registered=true`、固定 ePDG/Surge 配置未变、关闭 VoWiFi/断开读卡器/进程退出/私有 runtime 删除均成功、`error_code=null`。随后 `build/reports/iteration-559-full-build-inplace-roundtrip.json` 使用同一正式产物在强制硬件/外连沙箱中复测旧 3.42 → 新版 → 旧版 → 新版：数据往返与新版模块隔离、两次新版正常退出均通过；旧 3.42 两次仍退出 1，所以报告如实标记 `passed=false`、`full_p6_acceptance=false`。未运行 UI、通话、短信或长期稳定性测试，也未生成 DMG。

进一步对相同正式二进制使用**私有合成目录中的两枚旧模块空锁**进行一次实卡探测。`build/reports/simworld-560-quarantined-reader-live.json` 同时确认 `module_quarantine_confirmed=true` 与 IMS `registered=true`，且代理/ePDG 条件不变、四项清理再次成功。这证明本次合成旧锁触发的模块隔离没有挡住实卡读卡器注册；它没有读取或修改已安装应用的真实数据目录，也不证明历史模块物理状态安全。

测试后删除不再需要的独立诊断 target、共享 Rust Debug/Release target 和当前 Xcode DerivedData 缓存；保留源码、签名应用、旧 3.42 基线、受测隔离候选及上述验收报告。
