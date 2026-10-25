# Iteration 285：恢复任务丢失release响应但proof确认

日期2026-10-09，Switch_MAC。

共用实际恢复fixture新增LostConfirmation：服务读取并校验完整release原token/path后，以service error不发送响应；恢复任务仅追加原token release-status查询，受控proof true后完成原generation slot，不再次发送release。共一次acquire、一次release、一次只读query。旧generation恢复在replacement预留后拒绝，replacement snapshot保留。

清理/释放完成proof在fixture中受控，不声称真实Go物理release完成；测试覆盖实际Rust恢复Task、query客户端、supervisor完成及新代隔离。38项session模块测试重跑通过，日志../build/optimization/iteration-285-targeted.log。284完整门禁通过，早于本轮fixture改动。

main owner接管和真实跨语言迟到回调仍未验收，31.4%，无实卡/外部短信/DMG。
