# Iteration 280：释放等待预算覆盖Go PC/SC回调

日期2026-10-09，Switch_MAC。

追踪真实代码依赖：Rust NativeReaderSessionLease.release POST到Go私有session-lease/release；Go readerManager.releaseNativeReaderSessionLease同步调用readerPCSCTransaction.release，后者RPC到Rust内部reader-pcsc/release，最多130秒等待物理清理；Rust handler关闭期只允许status/release/close，实际release等待同binding结束。main仅Go terminate期间泵内部cleanup listener，接管时必须把原生清理与这段回调泵组合，不能先让listener空闲再独自await原生释放。

发现Rust release原本共用LEASE_TIMEOUT16秒，短于Go下游130秒，可能先超时且丢失晚到确认。改为单独RELEASE_TIMEOUT135秒，覆盖下游等待加5秒余量；acquire/renew仍16秒，AKA40秒、eSIM finalize100秒保持原值。没有改变Go代码、释放响应语义或号卡内容。

10项reader_session_lease测试通过（真实Unix IPC确认/断连/恢复、鉴权、heartbeat），日志../build/optimization/iteration-280-targeted.log。预算依据为当前源代码；未运行超过16秒的迟到回调测试或真实物理释放。连接断开后确认可能丢失、原Go lease已移除而再次请求409的恢复语义仍需解决，不能将135秒预算当完整解决。

main接管及桥回调泵顺序仍未完成。整体31.4%，无实卡/外部发信/DMG。
