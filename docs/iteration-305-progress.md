# Iteration 305：模块 SIM/APDU 跨业务 owner 核对

核查 Go `moduleSIMMu` 的全部生产调用后，确认它不是单纯的串口读写锁：当前覆盖 `runATCommand`、模块拨号/接听/DTMF、prompt 发送、SIMworld VoWiFi 的 USIM/ISIM 读取与 AKA、USB 短信发送、短信存储操作、完整设备状态查询及 AT console。USB 路径还持有 Rust USB-AT session lease 和 `voiceUSBTransition`；串口路径调用 Rust modem actor。Rust modem actor 只串行单条 AT 命令，USB-AT owner 只串行其自身 handle，两者没有共同覆盖多 APDU/多命令的 lease。

结论：本轮不单独删除/替换 Go mutex。那会让认证 APDU、短信的 CMGF/CMGS 序列、Profile 状态查询或 VoWiFi SIM 验证可被其他业务插入；如果 Rust lease 再嵌套在现有 Go 调用链，还可能形成 Rust public request → Go adapter → Rust actor 的锁顺序死锁。下一生产接管应是一个 Rust 物理 SIM arbiter，统一覆盖 USB 与 serial 传输，并迁移所有持有者和回调适配器；每个 lease 要绑定 owner/generation/token，取消等待可撤销，提交后的任务只有真实 I/O 退出后才释放，关闭/超时不能伪造释放。

离线验收门槛已明确为：并发交错测试覆盖 AT console、status、SIM APDU、AKA、USB 短信发送及存储操作；覆盖等待取消、owner 替换、Go 子进程退出、USB/serial 真实 I/O 超时和 stale release；证明一个物理卡不会跨两个事务交错命令，且不自动重放不确定写入。实机/长稳仍需独立验收。

这是 P1 owner 边界核对，不是代码接管或实机证据；总体加权进度保持 **31.4%**。无卡片 I/O、短信、通话、运营商访问或 DMG。


## 本轮验证补记

完整 `scripts/test-migration-gates.sh` 通过：Rust workspace **1,194 passed / 3 ignored**；Go 全仓 `go vet` / `go test` 及 carrier、USB status、radio、HTTP method/HEAD、PCMA历史helper、SDP、AMR和IMS MESSAGE parity 均通过。由于脚本固定请求 Go 1.26.3 且在线下载失败，使用 Switch/build/tmp 下的一次性 shim 调用本机 Go 1.26.7，脚本退出后已清理。此为当前离线回归，不改变 SIM/APDU owner 状态或总体进度。
