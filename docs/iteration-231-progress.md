# Iteration 231：注册关联的 MESSAGE 安全参数

日期：2026-10-09。工作区 Switch_MAC。

RegisteredImsSession现在在实际成功注册路径保存message_local_port及可选message_security_verify，提供只读getter。plain路径来自RegistrationPlan.plain_local_port，无Security-Verify；protected路径从当前已安装sec-agree上下文读取local_port_c/security_verify，绑定到最终成功的同一个association。Security-Verify保存为Zeroizing String，无日志或Debug输出，退出随session释放；安全SA lease及已有TCP先关闭后SA清理顺序保留。

实际内存网络、受保护注册协调集成测试核对成功session的local port5064和ipsec-3gpp安全头可读取。VoWiFi crate169项单元、2项集成通过。测试中手工会话fixture补齐字段，不能把fixture当作新运营商验证。

完整日志：`../build/optimization/iteration-231-migration-gates.log`：Rust workspace 1661项测试、格式、Go全仓test/vet及全部既有协议差分（含54组routing）通过，最终 `migration gates passed`。这些metadata仍需与注册身份/Service-Route、PANI和local IP整合到daemon session启动，公开生产loop尚未接管。整体31.4%，无实卡/运营商测试、实际发信、应用重启或DMG。
