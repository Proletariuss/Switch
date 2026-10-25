# Iteration 283：只读释放确认查询协议与Rust客户端

日期2026-10-09，Switch_MAC。

Go兼容桥新增POST /api/internal/reader/session-lease/release-status，沿用私有lease输入，格式无效返回400；合法令牌返回released bool，未知/过期false，仅此前真实显式完成的proof true。handler不获取/修改nativeLease或再执行物理release。原release API保持不变。

Rust NativeReaderSessionLease.release_confirmed通过原Go socket、原app token、原lease_id查询，16秒只读预算，true/false保留，不把错误当成功。此方法尚未接入release失败后的实际恢复判断；下一步须验证丢失响应/查询失败/旧桥404时原错误保留。

Go专用proof及query handler测试通过，Rust cargo check通过（既有warnings仍存在）。日志../build/optimization/iteration-283-go-tests.log与iteration-283-rust-check.log。完整门禁iteration-283-migration-gates.log通过。只读查询真实Unix IPC、private route边界与Rust调用组合仍需验收。

兼容桥临时协议未减少Go依赖，最终必须由Rust物理owner承接确认记录并删除Go实现。31.4%，无实卡/外部短信/DMG。
