# Iteration 484：普通会话恢复与公开 API 接线

本轮实现 daemon 持有的长期 intent、真实读卡租约/IMS adapter、绑定物理与会话代次的公开控制以及分阶段状态投影。生产接管仍受三个显式 readiness 门控限制；普通 IMS 与 N待继续保留既有 Go 路径，本轮不增加 Go 退出计分。

## 实现与交叉审阅

- 每次尝试绑定 reader、物理 generation、ICCID、IMSI 和原始代理/ePDG 请求；UI 请求取消不会丢弃启动和清理任务。掉线恢复使用 2/4/8/16 秒退避；续期与计划重认证区分记录，只有原资源退出获得确认后才能重试。
- 全局关闭与服务创建/启动采用同一准入锁顺序，阻止关闭后晚到启动；旧记录及原生会话代次不能控制新会话。准备失败、任务异常和读卡器变化都等待精确租约清理。Released 观测之后还必须收割原 worker 的实际完成结果，不能仅凭状态提前确认退出。
- 续期到期时阻止新增业务，保留已有通话的挂断、拒接、DTMF 与 USSD 收尾控制。待存量业务排空后再退出会话。
- API 使用真实 registered dispatcher，保留已知 SIP 拒绝状态；即使 USSD 拒绝响应的 ACK 写入失败，等待者仍收到已确定的 403，actor 同时保留原始清理错误，不重放请求。
- eSIM 下载准备阶段只停止匹配 reader/generation 的原生 intent；未确认释放则不开始下载。默认关闭的 native gate 不截获现有模块或 Go 普通会话路由。

## 验证边界

本轮定向测试使用隔离数据、模拟网络与无硬件读卡器，未执行真实卡片操作、短信、电话或运营商注册。完整迁移门禁已通过：Rust workspace **2423 passed / 0 failed / 6 ignored**（daemon 1727/0/3）、Go 全包 test/vet、Swift BLE 38 组、2726 项 carrier replay 及全部协议差分通过。USSD ACK 失败先红后绿证据、资源释放与 worker join 窗口的定向回归均保留于 `../build/reports/iteration-484-*.log`。发行门禁日志：`../build/reports/release-3.24/migration-gates.log`。

契约人工审阅范围由390增加至398：刷新7个实际变化范围、新增8个审阅范围；340个契约（Rust304/Go36）、127个Swift调用点及51个HEAD别名的归属保持不变。契约审计通过，证据 `../build/reports/release-3.24/contracts.log`。

## DMG 交付

按用户要求，从本轮源码生成 **3.24 / build324 / arm64 / macOS27.0+**：[Switch-3.24-macOS27.dmg](/Users/proletarius/Desktop/Switch_MAC/dist/Switch-3.24-macOS27.dmg)。大小 55322791 字节，SHA-256 `1306c82d0429df8b24397b8da8b8c1c5d06c6a2e2261c7b029b3a78f498e7b00`。

Release 构建、hdiutil 校验、只读挂载、codesign deep/strict、4个arm64 Mach-O分别验证、13份许可文件及 Applications 链接检查通过；镜像中的全部文件内容、权限和链接与签名后 `dist/Switch.app` 一致。本机 ad-hoc 签名，未公证，未安装或启动 App。

构建期间编译输入无漂移，仅两份契约审阅/清单 JSON 收口变动被单独记录；发行验证后更新进度文档和 README。证据 `../build/reports/release-3.24/{migration-gates.log,contracts.log,build.log,package.log,verification.json,source-after.json}`；原 DMG 和本地 archive 副本 SHA-256 相同。

## 尚未完成

incoming listener 状态/接收计数/可达性诊断目前仍未接入实际只读观测；Swift 共用调用别名的按来源分流、原生短信发送入口、全部冲突操作准入、N待接管及证书验证路径仍待完成。JSON 超限或部分非法 UTF-8 输入保持 Rust 更严格的边界，不宣称任意输入完全等价。所有接管门控保持 false；本轮不是全 Rust 完成版，38.31%仅保留为此前审计的历史估值。
