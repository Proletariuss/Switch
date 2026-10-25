# Iteration 232：可独立持有的注册消息快照

日期：2026-10-09。工作区 Switch_MAC。

RegisteredImsSession.message_context从自身成功REGISTER响应和成功association metadata构造Owned RegisteredMessageContext：注册消息身份/Service-Route、local port、可选Security-Verify。可在session移交actor之前捕获；只复制必要metadata，没有socket/SA lease复制，也不延长真实会话资源归属。敏感String继续Zeroizing，无Debug。

NativeSmsRouting.session只额外接收daemon generation、CSeq floor、local host及模板PANI；port/security来自该注册快照。旧registered helper仍作为snapshot内部及离线fixture的底层构建接口，不是生产接管。

实际内存TCP受保护注册测试通过成功session捕获snapshot，并比对port和Security-Verify与实际session相同、注册身份有效。公开事件循环仍需持有snapshot并调用daemon构建入口；本轮没有将原生启动门禁设为ready。

完整门禁：`../build/optimization/iteration-232-migration-gates.log`：Rust workspace 1661项测试、格式、Go全仓test/vet和协议差分（含54组routing）通过，最终 `migration gates passed`。整体31.4%，无实卡/运营商、实际发信、应用重启或DMG。
