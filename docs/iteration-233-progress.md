# Iteration 233：网络控制对象组装注册短信消费方

日期：2026-10-09。工作区 Switch_MAC。

NativeImsNetworkControl.registered_sms_consumer核对permit ReaderSessionKey与自身key、stop和lease状态，再从传入的成功RegisteredImsSession捕获message_context，并通过显式共享transaction service/session id取得NativeReportOwner。Consumer代次、停止及lease信号直接来自这个permit/control，不再由另一组手工信号传入。

新增RegisteredConsumer持有上述snapshot、local host/PANI及CSeq floor，process每次从自身固定metadata构造NativeSmsRouting；调用方不再逐条传路由snapshot。它保留SessionConsumer pending报告管理与disconnect输出。metadata拥有者没有复制socket/SA；真正IO/cleanup仍由actor控制。

这只是gated启动的组合方法，尚未由公开启动路由调用。传入session必须由该control.registration_services完成注册；目前组合方法并不额外证明任意session引用的TCP registry来源，因此不能宣称解决全部跨tunnel错误来源。local host/PANI仍来自正确profile/tunnel setup，完整worker-loop、actor与consumer共同收尾及端到端组合测试仍待完成。

完整日志：`../build/optimization/iteration-233-migration-gates.log`：Rust workspace 1661项测试、格式、Go全仓test/vet及协议差分（含54组routing）通过，最终 `migration gates passed`。本轮验证组装代码编译、既有实际内存注册及consumer离线回归；没有新增实机组合证据。整体31.4%，无实卡/运营商、实际发信、应用重启或DMG。
