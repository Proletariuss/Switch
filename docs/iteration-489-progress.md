# Iteration 489：原生状态、维护仲裁、心跳与 3.29 发行

本轮将读卡器选择、刷新和外部身份准备改为 Rust 原子维护凭据校验；聚合状态及短信设备列表采用原生会话与完整号卡身份事实，拒绝迟到的旧状态。普通 VoWiFi 全局停止按精确会话凭据执行一次并等待真实退出；补齐原生 TCP/OPTIONS 心跳、超时确认、就绪状态撤销和有界事件排队收尾。

普通会话心跳使用成功 REGISTER 冻结的配置和同一个 TCP registry。原生与兼容路径共用失败确认策略：主探测失败撤销新短信与呼叫就绪，延迟确认成功后恢复，确认失败触发会话清理。SMS-only 会话保持既有主动 OPTIONS 策略。事件读取仍为单一 owner；终态排队最多等待5秒，未投递事件按同代结果保留，不无限占用卡片租约。

交叉审阅修正了两处并发边界：状态渲染后身份变化会同步撤销旧注册状态；准入锁不可读时，新增短信和呼叫拒绝授权，退出观察仍保守持有旧资源。已停止会话的诊断信息保留；新 Go 会话真正取得准入后可恢复兼容观测。SMS subscription ID 与暂时就绪状态分开，续期和心跳确认期间不丢失稳定线路身份。

完整离线门禁通过：Rust 2577 passed / 0 failed / 6 ignored（daemon 1866/0/3），Go 26个测试包及 vet、Swift BLE 38组、2726项 carrier replay和协议差分通过。最终版本同步后 Rust workspace 再验通过。契约342（Rust305/Go37）、423审阅范围，127个 Swift HTTP 调用点和51条 HEAD 别名。

专项包括 service 57、快照12、短信设备16、reader lifecycle32、ordinary admission22、lease28、实际本地保护 TCP 心跳6、TCP22和 IMS actor13项，Go精确停止 race 回归通过。这些专项包含于全量或单独边界检查，不重复累加迁移完成率。

三个原生接管门控仍关闭；默认接管、生产组合链路、N待迁移、剩余安全套件、Go 退出和实机验收仍待完成。本轮没有新增 Go 退出计分，38.31% 仅为此前历史审计估值；新增私有 Go 精确停止兼容接口不代表 Go 依赖已退出。未操作真实号卡、呼叫或短信。

## 发行记录

应用户要求生成 Switch 3.29 / build 329 DMG，arm64、macOS 27.0+，55,407,879字节，SHA-256：`a51b7529be4d73b846ae75071ee68f3ce4f91835d77fb041f099331d26501496`。本机 ad-hoc 签名，未公证。

[下载 DMG](/Users/proletarius/Desktop/Switch_MAC/dist/Switch-3.29-macOS27.dmg)。App 版本、构建号和 Rust workspace 3.29.0 已统一。冻结4662个源码文件，最终构建输入无漂移；发行后仅更新本记录、README及两份计划说明。镜像校验、只读挂载、签名和签名产物文件一致性核验通过；独立复核结果见同目录报告。

证据：`../build/reports/iteration-489-migration-gates-final.log`、`../build/reports/release-3.29/final-rust-workspace-tests.log`，以及 `release-3.29/` 内的 `gate-summary.json`、`contracts.log`、`verification.json`、`independent-verification.json`。本轮 Rust-only 退出门禁仍按预期被145条 Go 所有权或依赖路由阻断。
