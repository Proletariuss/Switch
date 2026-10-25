# Iteration 252：启动到短信worker连续所有权

日期：2026-10-09，Switch_MAC。

spawn_sms_session消费唯一context与permit，复用已有不abort的Task，将原生tunnel/network/register成功结果直接移交spawn_registered_sms并等待其finish。放弃等待handle会请求同代次停止，内部任务继续注册rollback及worker退出。返回类型分别保留启动失败、worker JoinError和worker内部清理结果，不伪装为成功。

supervisor只持Weak；注册完成后若服务消失，不创建替代supervisor，也不启动短信consumer，而是等待已注册IMS shutdown、再请求IKE停止并await network，ServiceGone保留双方结果与原permit。任何启动失败返回permit由上层决定后续处理，未自动释放槽位。

cargo check通过（既有warnings仍存在）。完整门禁日志../build/optimization/iteration-252-migration-gates.log，完整门禁通过。公开路由仍未调用该组合入口，未完成真实SIM/IKE/IPsec/双向短信验收；外部panic恢复与失败slot重新清理仍需实现。整体31.4%，无实卡/外部发信/应用重启/DMG。
