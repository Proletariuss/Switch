# Iteration 219：原生会话任务取消与后台完成登记

日期：2026-10-09。上一轮取消/unknown语义改动已完成验证；本轮继续修复任务所有权。所有代码和验证日志在Switch内。

## 改造

`native_ims_actor::Task`封装真实JoinHandle，调用方只得到finish入口，没有abort入口。丢弃句柄或取消finish等待只对本代次发送停止信号，实际owner任务继续执行IMS/网络退出；不会因等待方消失而跳过清理。

`NativeSessionPermit`持有该代次的取消能力。Supervisor销毁时显式停止所有残留会话，即使Task仍保留watch sender也不会因信道仍开而继续运行。旧取消sender只能影响旧代次channel，不能停止替换后的会话。

`NativeImsNetworkControl::spawn_registered_actor`接收共享supervisor，任务自身完成mark_running与worker_finished：退出时先等待IMS cleanup，再停止IKE并等待network task，随后登记对应代次完成。完成登记不依赖调用方等待Task结果。任务只弱引用supervisor，避免任务本身阻止控制者销毁和停止广播。启动登记失败则请求停止并清理，不启动额外IO；supervisor已销毁或锁损坏时完成状态为None，不伪造释放结果。

## 验证

新增3个测试（首项分别覆盖丢弃句柄及取消等待）：后台清理不会被等待方取消，清理完成前槽位仍占用、之后后台释放；supervisor销毁时即使取消sender仍存活也停止；旧任务取消不影响replacement generation。actor定向8项全部通过，含之前的代次/背压/lease测试。

完整门禁日志：`../build/optimization/iteration-219-migration-gates.log`；定向日志`iteration-219-owner-tests.log`。完整Rust workspace/格式、Go全仓test/vet及既有协议兼容性门禁通过。

## 剩余

此生命周期封装已接到真实Rust网络控制对象，但生产公开启动入口、完整原生事件循环与SMS/call能力门禁仍未启用。进度保持31.4%。没有把离线清理测试作为实卡或全Rust运行完成证据。本轮无设备、实际短信、运营商连接或DMG。
