# Iteration 532：主叫初始 INVITE 的 422 重试

2026-10-18。Rust IMS 通话 owner 现可处理尚未建立早期对话的初始 INVITE 收到 `422 Session Interval Too Small`：先 ACK 旧事务，再按 `Min-SE` 提高 `Session-Expires`，沿用 Call-ID、From、To 与 SDP，以新 Via branch 和递增 CSeq 发起下一事务。逻辑通话与媒体 worker 的原始绑定不变，避免重试把既有音频资源变成无法关闭的旧代。旧 422 重传只重发原 ACK；最多重试四次。缺失、无效、不增长的下限、用户已取消或已建立带标签早期对话时不自动重拨，而按既有终局处理。最终 200 若带低于已要求 `Min-SE` 的间隔，ACK 后 BYE 并回收媒体。

此行为按 [RFC 4028 §7.3–7.4](https://www.rfc-editor.org/rfc/rfc4028.html) 的新事务及 CSeq/Min-SE 要求实施；参数化 `Min-SE` 的数值部分可解析。定向回归覆盖递增的多次 422、旧响应重传、错误下限、重试预算、取消竞争、早期对话与最终 200 下限。

完整 Rust 工作区 **3060 通过、0 失败、1 忽略**；生产 `switchd-rs` 开发构建、格式与差异检查通过。公开接口盘点仍为 **294/294 Rust HTTP owner**，127 个 Swift 调用点与 430 个审阅范围保持；仅因文件顶部新增常量刷新了三个已核对函数的行号。Go 退出静态门禁通过，未发现生产构建、嵌入、启动或分发路径的 Go 边；这些静态检查不代替实机和旧数据验收。记录在 `src/build/reports/iteration-532-rust-workspace.log`、`iteration-532-ims-focused.log`、`iteration-532-daemon-build.log`、`iteration-532-contracts-final.log` 与 `iteration-532-go-exit.log`。

本轮只修改 Switch_MAC 源码与离线测试；未连接或更改实体卡，未改变代理规则，未发送短信、拨号或生成 DMG。对不支持 UPDATE 的对端改用 re-INVITE 刷新仍未完成；SIM World 的 ESP 回程与 IMS 实机注册也尚未验收。
