# Iteration 244：注册快照的本地 IP 来源

日期：2026-10-09。工作区 Switch_MAC。

RegisteredImsSession成功路径保存RegistrationPlan.local_ip，protected注册将同一plan IP传入受保护协调完成路径，连同既有port/security metadata进入Owned RegisteredMessageContext。不是从入站短信学习IP，也不是自行解析ePDG域名；真实native profile的plan仍应由认证后的tunnel configuration解析。

RegisteredConsumer创建时从snapshot.local_ip生成local_host并固定持有，NativeSmsSetup删除手工local_host参数，daemon组合入口不再允许另一段字符串覆盖Via地址。仍须由正确网络控制对象创建注册session，未把保存plan IP当作任意socket来源的额外密码学证明。

既有真实plain内存TCP组合和protected注册协调测试新增snapshot IP=10.0.0.2核对，继续核对真实port/security metadata及报告wire。完整日志：`../build/optimization/iteration-244-migration-gates.log`：Rust workspace 1675项测试、格式、Go全仓test/vet及协议差分（含54组routing）通过，最终 `migration gates passed`。

整体31.4%，公开启动、AKA/IKE/IPsec全链和持久report恢复仍未完成。无实卡/运营商、实际外部发信、应用重启或DMG。
