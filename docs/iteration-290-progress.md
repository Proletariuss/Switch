# Iteration 290：daemon 原生生命周期接入

2026-10-09，源码与构建仅在 Switch_MAC 中。

ServerState 与全部测试构造器接入默认空 NativeLifecycle。退出先关闭原生准入，在 Go 桥仍存活期间驱动两条已鉴权清理监听器，等待原生 owner 完成，再退出 Go。未确认及正在关闭的结果继续等待，180秒只是单轮观察期限，不构成资源释放证明。其他失败保留错误并沿既有退出收尾传播。

生产原生会话安装仍关闭；锁异常等致命退出路径的完整恢复策略、实际跨语言租约释放与实机退出验证仍待完成，不能据此开启生产接管。31.4% 保持不变，无实卡操作、发信或 DMG。

Iteration289 完整门禁已通过。Iteration290 初次编译发现测试构造器字段遗漏，已补齐；定向退出回调测试已通过，日志 ../build/optimization/iteration-290-targeted.log。完整迁移门禁已通过，日志 ../build/optimization/iteration-290-migration-gates.log，进程退出码0。
