# Iteration 516：通话中 UPDATE 媒体更新

2026-10-18。全 Rust 后端目标保持 active。本轮处理 Rust 语音会话中一个实际的 SIP/媒体不一致：原先带 SDP 的 UPDATE/PRACK 即使改变 RTP 目标，只要编码名称相同就可能得到空体 200，而媒体 worker 仍向旧端点收发。

## 实施

- 同一通话的带 SDP UPDATE/PRACK 现在校验 `Content-Type: application/sdp`、已有媒体 socket、活跃/早期媒体状态和不变的 EVS/AMR-WB 编码配置。无法支持的媒体请求返回 415/488，不再空体确认。
- 对可接受的同编码 offer 返回实际 SDP answer；保留原始 `o=` session ID 并将 SDP 版本递增。响应写入成功后在原 generation/call binding 的媒体 owner 中切换 RTP 地址、端口及 payload type，保留原音频设备/会话。写入或媒体更新失败时终止该通话的媒体，不发布虚假的新媒体就绪。
- 媒体 worker 切换对端时清理旧 RTP 重排/序号观察状态，并使正在发送的 DTMF 明确失败，避免旧端报文混入新端。

## 验证与限制

| 证据 | 结果 |
| --- | --- |
| Rust 工作区 | 3025 通过、0 失败、1 忽略；见 `build/reports/iteration-516-rust-workspace.log` |
| 活跃通话 UPDATE | 同编码新端点返回 200+SDP、版本增加、媒体 owner 更新；错误媒体类型/不支持编码返回 415/488，且不改动媒体 |
| 真实媒体工作线程 | 同一音频桥保持，后续 RTP 仅抵达新端点；新端点 RTP 能解码回主机；见 `build/reports/iteration-516-media-update-real-worker.log` |
| Rust 构建与格式 | 开发后端构建、Rustfmt、差异检查通过 |
| 接口归属 | 294/294 Rust HTTP owner、0 Go HTTP owner；本轮未新增接口 |

本轮**没有完成** RFC 4028 通话会话定时器、完整双向 re-INVITE、UPDATE 的远端 target refresh、跨编码/CMR 模式切换及运营商实测。初始 SIM World 实卡仍停在上一轮记录的 IMS 连接超时，未具备通话实测前提。9eSIM 6982 继续排除。未操作卡片、发信、拨号或生成 DMG；已安装应用没有更新。
