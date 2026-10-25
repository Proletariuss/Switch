# Iteration 488：原生读卡器准备、VoWiFi 配置与 3.28 发行

普通原生读卡器准备现在使用同一准入凭据验证身份与代次，不再依赖 Go producer 或查询 Go 身份状态。任务取消仍由保留的实际任务完成清理；维护、旧业务和 N待占用继续阻止并发启动。外部 Identity 和 Mutation 路径的兼容行为保持。

VoWiFi 原生 HTTP 路径补齐请求错误 400、占用 409、无卡 503 和配置错误 500 的区分；保留旧接口的端口默认与错误优先级。未启动状态通过当前读卡器快照生成配置预览，预览结束后再次检查身份与代次；预览不取得卡片租约或发起网络会话。真实启动和状态展示共用配置解析，已启动会话展示冻结的配置。显式配置路径仍优先于默认数据目录，成功预览不显示空的错误提示。

这轮没有启用尚待验收的原生默认接管门控，没有新增 Go 退出计分，也没有进行真实卡片、通话或短信测试。Go 后端仍为发行包运行依赖。38.31% 仅为此前已审计的历史估值，不能由测试数量推高。

## 尚待推进

默认接管前仍需补齐聚合状态与短信设备列表的原生事实来源、模块/global Stop 边界、原生 TCP/OPTIONS 心跳配置消费，并核对普通重建失败的终止策略与原行为。已有实际 IKE、IMS、短信、通话与 RTP owner 接线；仍需生产组合路径、剩余安全套件兼容和真实设备/网络验收。N待执行与剩余 Go 退出继续按项目目标推进。

## 发行验证

本轮应用户明确要求生成 [Switch 3.28 / build 328](/Users/proletarius/Desktop/Switch_MAC/dist/Switch-3.28-macOS27.dmg)，arm64、macOS 27.0+，55,416,283 字节。SHA-256：`ee494ae79781920161e5d78115ac165f5c4e7a18c101324e39e00e4cf6d96b0a`。本机 ad-hoc 签名、未公证；所有源码与产物均在 Switch_MAC 范围内。

完整迁移门禁通过：Rust **2540 passed / 0 failed / 6 ignored**（daemon 1836/0/3），Go 26 个测试包及 vet，Swift BLE 38 组、2726 条运营商回放及协议差分。专项包括读卡器无 Go 准备 6 项、完整 lifecycle 30 项、lease 28 项、配置 8 项、ordinary 14 项、service 45 项。契约三种检查通过：341 条（Rust 305 / Go 36）、127 个 Swift HTTP 调用点、51 条 HEAD 别名、417 个审阅范围。

Release 构建、签名、镜像和只读挂载核验通过。独立复核确认 30 个 App 文件的内容、权限和链接与本次签名产物一致，4 个 Mach-O 均为 arm64，运行依赖无开发机路径，13 份许可内容匹配，归档副本 SHA-256 相同。冻结的 4655 个源码文件在构建时仅变更两份契约审阅元数据，无编译输入漂移；发行后只更新本轮进度、README 和两份计划说明。

证据位于 `../build/reports/iteration-488-migration-gates-final.log` 与 `../build/reports/release-3.28/`，包含 `gate-summary.json`、`contracts.log`、`verification.json` 及 `independent-verification.json`。
