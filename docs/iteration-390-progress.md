# Iteration 390：REGISTER registrar 候选推进策略迁入 Rust

REGISTER 失败后是否尝试下一个 P-CSCF registrar 候选现由 Rust IMS codec 决策，并接入生产读卡器 IMS 会话。Go 只把已观察到的状态码、剩余候选事实、carrier forbidden/temporary 状态列表及受限 probe failure 类别提交给 Rust，继续负责建立候选连接、等待、SIP socket/事务、AKA、IPsec 与 IMS session 生命周期。Rust RPC 缺失、输入不合法或响应格式异常时生产路径失败关闭。

Rust 保留默认 forbidden 状态 403 与默认 temporary 状态 408、500、502、503、504、480；模板显式空列表仍禁用默认集合，模板自定义列表按原顺序语义覆盖默认值。探测类只允许 deadline、canceled、partial SIP、超时文本、transaction ended、REGISTER response timeout 和连接关闭类别推进；未知类别拒绝推进。无更多候选时恒不推进。

验证通过：Rust policy 单测 **5/5**，daemon codec 定向测试 **1/1**，Go imscore 与 macOS backend 定向测试通过；完整 `scripts/test-migration-gates.sh` 通过，覆盖 Rust workspace、Go 全仓 test/vet、Swift BLE/manager 回归和现有协议差分。日志：[iteration-390-migration-gates.log](../build/optimization/iteration-390-migration-gates.log)，SHA256 `cdb73fcd249edcd678176a1a843656a07dc7e605ac3fb67e8b5468b96a442cd7`。契约 `--diff` 通过：343 method/path（Rust 283 / Go 60）、127 Swift HTTP 调用点、359 审阅范围；没有新增公开 HTTP 路由。没有连接读卡器/SIM、没有真实 IMS/短信/呼叫测试，也没有生成 App/DMG。

P3 +0.025 计划人周等值，累计 **2.465 / 8.0 = 30.8125%（显示30.8%）**；整体迁移累计 **16.945 / 47.5 = 35.6737%（显示35.7%，两位小数35.67%）**，工程估值误差约 ±3 个百分点。仍未完成 IMS 网络事务、SIP socket、IKE/IPsec、注册会话生命周期以及实机/运营商验收。
