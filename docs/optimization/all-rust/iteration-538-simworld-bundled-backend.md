# Iteration 538：应用包内 Rust 后端的 SIM World 实卡复测

日期：2026-10-18。

用户确认先测试 ESTKme-RED 中已启用的 SIM World。使用上一轮 Release 构建的 **`Switch.app` 包内 `switchd-rs`**，在独立 `SWITCH_DATA_DIR` 下运行，不替换已安装应用。新增 `scripts/probe-reader-vowifi.py` 作为显式运行的实卡探针：要求读卡器名称和运营商匹配才尝试 VoWiFi，只输出状态转换与安全计数；结束时停止会话、断开读卡器、退出 daemon。脚本不提供切卡、下载、短信发送或拨号动作。

首次测试把 `CFFIXED_USER_HOME` 也指向空目录，读卡器读取成功而 VoWiFi 启动返回 500；该设置使当前 Surge 用户配置不可见，不代表卡片认证失败。仅隔离 Switch 数据目录、保留系统代理用户环境后，包内后端成功连接 SIM World 454/03，启动返回 200，隧道依次 `connecting → connected`，IMS `idle → registering`，最后 `error/failed`。保留的诊断为 `ims_transport / transport_failed / plain_connect / timed_out`；ESP 本地发出 **8**、收到 **0**、拒绝 **0**，CHILD SA 后 IKE、NAT-T 保活和无效数据报入站计数均为 **0**。`sms_ready=false`，`call_ready=false`。Surge 该进程的会话记录选择香港 14 家宽，出站 2051 字节、入站 1188 字节；入站总量包含 IKE 握手，不能作为 ESP 回程证据。

本次证实当前发行包内 Rust 后端与开发版在实卡上到达相同阶段，也说明完整代理用户环境是复测的必要条件。它仍不能区分代理出口、ePDG 丢弃或数据面兼容性问题，**SIM World IMS 注册、短信与通话未通过验收**。在获得代理出口或 ePDG 侧 UDP 4500 接收证据、或同卡同网已知正常客户端对照之前，不修改运营商模板或 DNS 路由。测试结束确认 VoWiFi 停止、读卡器断开、隔离 daemon 零退出；本轮创建的临时运行目录已清理。未生成 DMG。

脱敏证据：`build/reports/iteration-538-simworld-sanitized.json`、`build/reports/iteration-538-surge-sanitized.json`。另以故意不匹配的运营商参数验证探针在读取后拒绝启动 VoWiFi，断开读卡器并自动删除独立运行目录，记录于 `build/reports/iteration-538-guardrail-sanitized.json`。探针通过 `python3 -m py_compile`、`--help` 检查；`git diff --check` 通过。本轮未修改应用生产逻辑。
