# Iteration 485：原生短信、通话来源及接入诊断

原生 reader IMS 短信公开入口接入既有持久化发送流程：绑定当前卡片与会话代次、唯一提交、单批次状态确认及结果归档。准备后发生换卡、续期、会话重建或绑定失效时拒绝旧操作；部分接受或结果未知不自动重发。Go 恢复期间，兼容路径仍在消耗发送授权之前返回原错误；已有 Rust 短信路径独立于 Go 可用性。

两组 VoWiFi 通话 API 依据当前会话来源分流。原生读卡器与模块归属冲突时拒绝控制，不以 URL 名称猜测来源；保持既有模块通话行为。Port-S 监听状态、接入计数、最近 SIP 时间与响应写入错误来自实际监听和连接事件；IMS 注册不再被作为入站可达性的证明。

完整离线门禁通过：Rust workspace **2451 passed / 0 failed / 6 ignored**（daemon 1754/0/3）、Go 全包 test/vet、Swift BLE 及全部协议差分通过。定向验证包括短信 37 项、来源路由 19 项、短信绑定 9 项、接入诊断 14 项、TCP listener 21 项、ordinary 14 项。测试采用隔离运行数据和模拟网络，未操作真实卡片、发送短信或拨打电话。

全 Rust 迁移仍未完成：原生与模块的跨路径原子准入、生产接管、N待接管、证书验证路径及实机验收仍待完成。三个生产接管门控保持关闭，Go 仍为运行与构建依赖；本轮不增加 Go 退出计分，38.31%仅保留为此前审计的历史估值。

## 契约与发行验证

契约审计通过：340 条路由（Rust 304 / Go 36）、127 个 Swift 调用点、51 条 HEAD 别名及 403 个审阅范围。刷新 9 个实际变化范围，新增 5 个生产代码范围；条件分流没有被统计为默认 Rust 接管。

按用户要求生成 **3.25 / build 325 / arm64 / macOS 27.0+**：[Switch-3.25-macOS27.dmg](/Users/proletarius/Desktop/Switch_MAC/dist/Switch-3.25-macOS27.dmg)。大小 55337515 字节，SHA-256 `94b3decccd0082ccb70b2a6522312da6ce4d08286fd4e5e3a577cdc6bbe2780f`。

Release 构建、hdiutil 校验、只读挂载、codesign deep/strict、4 个 arm64 Mach-O 分别签名、13 份许可文件及 Applications 链接检查通过。镜像中的全部文件内容、权限及链接与签名后的 dist/Switch.app 相同；本地 archive 副本校验相同。本机 ad-hoc 签名，未公证，未安装或启动 App。

构建期间编译输入无漂移；仅两份契约审阅/清单 JSON 收口变化已单独记录。验证后更新本记录、README 与计划/执行看板。证据：`../build/reports/release-3.25/` 中的 migration-gates.log、contracts.log、build.log、verification.json 与 source-after.json。
