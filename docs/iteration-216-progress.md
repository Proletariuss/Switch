# Iteration 216：原生IMS入站短信的持久归档凭据

日期：2026-10-09。上一轮为代码/离线验证进展。本轮所有源码、隔离存储及日志在Switch内。

## 变更

`ims_sms_transactions::archive_native_sms_event`接收Rust会话actor的类型化事件，校验当前generation、MESSAGE及唯一必需头、Content-Type、CSeq和准备上下文的subscription_id。复用当前Go适配路径所用的Rust解码/归档函数和真实ArchiveStore原子writer；未另建平行归档格式。

只有writer完成且`store_state=archived`时返回`NativeSmsCommit`。凭据字段私有，无Debug输出，并绑定当前session generation、原始Call-ID和RP reference。消费凭据准备RP-ACK MESSAGE时再次检查generation与In-Reply-To；写盘失败或无有效归档结果没有凭据。函数本身不写网络，取消它不会自动应答；底层已开始的持久写入可能仍完成，但调用方没有返回凭据，也不能凭此提前确认。

状态报告、无法解码和非SMS事件不生成普通短信归档凭据；相应完整处理策略仍需补齐。N待归档、完整重组、完整事件消费者、SIP成功应答与RP-ACK实际提交/重连恢复仍未在native actor中闭环。生产native readiness继续关闭，Go现有IMS owner保留。

## 验证

新增3项使用真实隔离ArchiveStore的测试：归档完成后才可准备带正确reference的RP-ACK；重复投递不增加归档条数；stale generation无写入；真实归档路径不可写时无凭据；替换代次或错误In-Reply-To无法消费凭据。

完整迁移门禁日志：`../build/optimization/iteration-216-migration-gates.log`。完整Rust workspace/格式、Go全仓test/vet及全部既有载波/USB/射频/HTTP/codec兼容门禁通过。

## 状态

此组件已复用实际生产归档writer，但尚未挂到生产会话actor。不能因此宣称native短信已接管或真实收短信验收完成。整体加权进度保持31.4%。下一步把按当前generation归档得到的凭据接入actor应答/发送命令，并完成入站业务、关停和重发矩阵。本轮无实卡操作、真实短信、运营商网络或DMG。
