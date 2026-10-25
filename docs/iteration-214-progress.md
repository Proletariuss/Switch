# Iteration 214：Rust IMS 事件接入短信提交状态机

日期：2026-10-09。上一轮为实际进展：有界响应分派代码和测试已落地。本轮继续补齐它与既有短信事务的衔接，全部变更在Switch内。

## 变更与边界

`SmsSubmission::observe_response_headers`接收已解析的状态码和头字段；原始wire入口也委托同一实现，避免克隆整条响应和短信正文。严格拒绝重复相关头、非法状态码、CSeq尾随字段、带正号或越界序号，错误输入不改变提交状态。

`RegisteredSipEvent::observe_sms_response`只将主连接上的响应传给短信状态机，其他连接、请求和超时事件忽略。响应票据已超时回收不妨碍原SMS事务识别迟到的精确Call-ID/CSeq/MESSAGE确认；原有OutcomeUnknown、LateResultResolved、多段序列和终态幂等语义全部复用。新旧会话之间的隔离仍须由下一步keyed actor保证；本轮未开启生产native gate，也没有接管Go IMS生命周期。

## 验证

增强双端内存TCP网络测试：发送后半帧响应超时，标记短信OutcomeUnknown；恢复解析迟到完整响应后转为LateResultResolved；重复确认忽略，下一次请求仍能完成，未自动重发。

新增两个定向测试：错误连接无法解除结果未知；三种异常CSeq拒绝且状态完全不变。VoWiFi定向完整测试163单测与2载波集成通过。完整Rust workspace/Go test/vet及协议兼容性迁移检查通过。日志为`../build/optimization/iteration-214-migration-gates.log`；新增两项测试单独覆盖在`iteration-214-vowifi-tests.log`中。

## 后续

下一步仍为keyed session actor与入站短信durable commit/RP-ACK顺序，之后逐项迁移生产owner。原加权进度31.4%保持不变；组件接线不算完整生产接管。本轮未操作实卡、发送真实短信、连接运营商或生成DMG。
