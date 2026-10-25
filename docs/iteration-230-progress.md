# Iteration 230：注册消息路由的直接差分门禁

日期：2026-10-09。工作区 Switch_MAC。

此前routing只有Rust单元测试和consumer组合fixture，没有直接对比生产Go identity选择函数。本轮新增离线message_routing_fixture：读取受限stdin JSON，构建REGISTER 200 metadata并调用同一Rust生产routing对象，不打开网络/读卡器。

Go TestRegisteredMessageRoutingRustParity运行该fixture，并直接调用原生产registeredMessageIdentity作为identity oracle；Service-Route按原responseHeaderStrings trim/保留顺序及fallback规则比较。9组P-Associated-URI×2个配置identity×3组Service-Route，共54组：空/invalid、tel/sip/sips、配置identity不在首位、显示名逗号、重复header、URI参数、route内部列表、header顺序及fallback。无fixture时普通Go test跳过；scripts/test-migration-gates.sh现在必须构建并提供Rust fixture，完整门禁不能跳过差分。

这是注册消息routing的有效格式兼容性证据，不是IMS注册、SIP写入、运营商短信或全Rust接管验收；Rust独立错误边界测试仍保留。报告协议生产路径无新增Go依赖，该Go测试仅迁移比较oracle，P6收口时保留冻结结果或改成纯Rust回放。

完整日志：`../build/optimization/iteration-230-migration-gates.log`：Rust workspace 1661项测试、格式、Go全仓test/vet、既有协议差分及新增54组routing直接差分通过，最终 `migration gates passed`。整体31.4%，生产完整事件循环未接管。无实卡/运营商、实际发信、应用重启或DMG。
