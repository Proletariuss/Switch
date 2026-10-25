# Iteration 281：超过旧16秒预算的真实Unix释放确认

日期2026-10-09，Switch_MAC。

新增实际NativeReaderSessionLease acquire/release Unix socket测试，服务收到原release请求后等待LEASE_TIMEOUT+1秒（当前17秒）才回复released=true。客户端整体测试deadline25秒，实际release必须成功且耗时大于旧16秒预算；旧客户端16秒timeout会失败，不能通过只改常量的镜像断言。服务只有一次acquire一次release，未自动重试。

定向通过（17.01秒），日志../build/optimization/iteration-281-targeted.log。此测试证明客户端17秒迟到确认，不代替真实Go/Rust PC/SC130秒物理清理；断连后确认遗失恢复仍未完成。31.4%，无实卡、外部短信或DMG。

最新完整门禁通过：../build/optimization/iteration-281-migration-gates.log。
