# Iteration 540：SIM World 当前 Rust 后端复测与 ESP 路径审阅

日期：2026-10-18。

按用户选择，继续测试 ESTKme-RED 中已启用的 SIM World。使用当前 Release `switchd-rs`、独立 `SWITCH_DATA_DIR` 和现有系统 Surge 代理；未切卡、下载 Profile、发送短信或拨号。探针再次确认运营商 SIM World、MCC/MNC 454/03。IKE/EAP 成功建立隧道，IMS 进入 registering 后在 `plain_connect` 超时：ESP 发出 8 包、入站 0 包、拒绝 0 包；CHILD SA 后 IKE、保活及无效数据报入站也均为 0。短信和通话未就绪。脱敏结果见 `build/reports/iteration-540-simworld-sanitized.json`。测试结束后 VoWiFi 已停止、读卡器已断开、隔离 daemon 正常退出，临时运行目录已清理。

审阅了 Rust 的 CHILD SA 密钥排列、AES-256-CBC/HMAC-SHA256-128 ESP 封装、IP 流量选择器和 SOCKS5/NAT-T UDP 4500 收发路径。当前协商套件的发包使用 initiator 方向密钥、远端 SPI，收包使用 responder 方向密钥、本地 SPI；CBC 的 16 字节 IV、块填充、ESP 头与密文的截断 HMAC 在代码上与既有独立线向量一致。既有测试还覆盖了合成 SOCKS5 UDP 4500 回程。结合本轮“所有 CHILD 后数据报入站均为 0”，没有证据把故障定位到解密、策略检查或运营商模板；也尚不能证明代理出口收到并转发了 ESP。保持现有代理分流和运营商配置，后续需要代理出口/ePDG 侧 UDP 4500 接收证据，或同卡同网络已知正常客户端对照，再定位零回程原因。

另发现已安装 `/Applications/Switch.app` 的旧后端进程仍在运行，但其本地 `/api/health` 在 3 秒内未响应；该二进制早于当前 Release 产物。SwiftUI 启动管理器会拒绝覆盖仍可连接的旧 socket，以保护可能存在的 IMS/短信/通话会话；旧进程因而可能挡住正式应用的下一次启动。当前隔离 daemon 的健康与实卡流程正常，所以这仅是**已安装旧实例的运行状态问题**，尚不能归因于当前源码。本轮未终止或替换已安装进程。原始进程采样仅用于即时审阅，未保留到源码或报告中。

修正 `scripts/probe-reader-vowifi.py` 的启动错误清理：缺失/不可执行二进制在创建运行目录前拒绝；系统启动失败会删除刚创建的目录。`py_compile`、缺失路径和无效解释器场景通过，`git diff --check` 通过。未生成 DMG；SIM World 的 IMS、短信、通话实卡验收仍未通过，项目整体进度本轮不增加。
