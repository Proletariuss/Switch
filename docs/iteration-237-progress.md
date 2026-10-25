# Iteration 237：组合 worker 退出屏障验证

日期：2026-10-09。工作区 Switch_MAC。

将上一轮实际spawn_registered_sms使用的join逻辑提取到join_registered_sms_workers，生产组合路径直接调用这一同一函数：等待actor completion（已包含网络收尾），请求同代次stop，等待consumer或保留setup拒绝码，最后按真实exit.permit登记supervisor完成。没有额外完成计数或替代fake路径。

新增2项离线并发测试，使用真实Task、NativeVoWiFiSupervisor、permit及watch/oneshot屏障：actor已结束后consumer尚在等待，stop已发出而slot不能reserve；consumer完成后才登记并允许reserve。setup拒绝时actor cleanup屏障未释放，join仍等待且slot占用；释放后保留错误码并登记。actor/network结果在这两项中为受控fixture，不是socket/运营商端到端测试；验证的是生产共享的任务等待和slot登记顺序。

完整日志：`../build/optimization/iteration-237-migration-gates.log`：Rust workspace 1666项测试、格式、Go全仓test/vet及协议差分（含54组routing）通过，最终 `migration gates passed`。真实注册+loop+网络组合测试、panic/持久恢复及voice/USSD接续尚未完成；公开startup仍关闭。整体31.4%，无实卡/运营商、实际发信、应用重启或DMG。
