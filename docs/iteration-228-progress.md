# Iteration 228：注册响应的消息身份与路由

日期：2026-10-09。工作区 Switch_MAC。

新增 RegisteredMessageRouting，并提供 RegisteredImsSession.message_routing 入口，从该真实session保存的final_response建立MESSAGE身份与Service-Route。遵循Go registeredMessageIdentity语义：配置identity若在P-Associated-URI中获认可则优先，否则第一个有效sip/sips/tel公共identity，均无有效值时保留配置identity。列表拆分识别显示名引号和尖括号；Service-Route按header顺序保留原值和内部列表，缺失时用caller提供的配置fallback。不从入站短信或未经注册的消息体学习身份。

路由snapshot无Debug、身份及route字符串用Zeroizing保存。只允许2xx构建，基本身份格式、空route、控制字符和既有8条route上限校验。NativeSmsRouting.registered直接借用这个注册结果，route类型改为String slice，不再要求caller重复拼接临时引用列表；local endpoint、安全头和PANI仍由对应真实transport/security/template提供。

新增注册metadata测试（被认可配置identity、首个identity、显示名逗号、route顺序、fallback和非成功拒绝）。真实ArchiveStore/模拟actor的consumer测试加入注册响应fixture：authorized身份与registered-route必须出现在最终RP-ACK的From/Route，实际短信PAI仍选择目标，序列75→76不变。fixture不是运营商连接验收。

完整日志：`../build/optimization/iteration-228-migration-gates.log`：最终源码重新执行全套门禁，Rust workspace 1660项测试、格式检查、Go全仓test/vet及协议差分通过，最终 `migration gates passed`。完整启动/事件任务仍未公开接管，注册snapshot方法的实际调用及安全参数来源仍需组装进session owner。整体31.4%，无实卡/运营商、实际发信、应用重启或DMG。
