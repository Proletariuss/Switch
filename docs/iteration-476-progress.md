# Iteration 476：修复通话模式锁交接并发布 Switch 3.16

按用户本轮要求生成 DMG，版本为 3.16 / build 316。全 Rust 后端目标仍在实施，本轮以修复迁移回归及发布核验为主，不因测试数量、私有接口或锁搬迁增加完成率：**18.145/47.5 = 38.20%（估算误差约 ±3 个百分点）**，P2B 4.975/8.0。

## 修复范围

此前 Go 通话模式操作持有独占文件锁时，Rust ADB 自检再次申请同一文件的共享锁，导致操作被自身拒绝。现在由 Rust radio reservation 持有真实独占文件锁；私有自检启动必须携带当前 producer、token、sequence，并借用不可伪造的类型化 guard。公开语音启动沿用共享锁与 readiness 校验，短时 radio 读取沿用普通 reservation。

借用 guard 受 radio admission gate 排他保护；原生 physical 入口检查活动 voice pin，避免刷新/动作与 ADB 自检并发。pin 被计入实际工作数量；即使 HTTP 请求取消或超时，独立 worker 仍持锁至实际 ADB 与 USB 清理完成，release/retirement 不会提前释放。Go 保留部署、恢复、watchdog 和高层通话模式生命周期，本轮没有宣称完整接管。

通话模式 retry 保留旧公开接口忽略请求正文的行为，移除新增的严格空 JSON 校验；六种请求体分别验证可重试与不可重试情况，避免兼容回归。

## 审查及验证

独立交叉审查发现并修正 unknown 状态借用、radio admission 的异步窗口以及 queued refresh 旁路；最终增量审查未发现 P0/P1/P2。Rust 原生锁/guard 专项 18 项通过，ADB 专项 30 项通过；Go 通话模式定向及 race 测试通过。

完整 `scripts/test-migration-gates.sh` 通过：Rust workspace **2,130 passed / 0 failed / 6 ignored**（其中 daemon 1,494 passed / 0 failed / 3 ignored）；Go 全仓 test/vet、Swift BLE 38 组虚拟事件回归、2,726 项 carrier replay 以及 USB/radio/HTTP/HEAD/IMS/SDP/AMR/MESSAGE/USSD 差分均通过。Release 构建成功。未操作真实读卡器/号卡、未发送短信或拨号；静态/离线验证不等于当前设备及运营商验收。9eSIM `6982` 不在本轮处理范围。

契约为 354（Rust 295 / Go 59，含 51 个 HEAD alias），127 个 Swift HTTP 调用点，381 个审阅范围；13 个生产范围完成手工审查并更新指纹，census `--diff` 通过。Go 结构退出审计仍有 6 类阻塞、265 个生产 Go 文件。

构建及测试产物保存在 `/Users/proletarius/Desktop/Switch/build/reports/`，源码和发行文件均位于 Switch 工作区。

## 发行校验

[Switch 3.16 DMG](/Users/proletarius/Desktop/Switch/dist/Switch-3.16-macOS27.dmg)，build 316，arm64，最低 macOS 27.0，53,298,348 字节。SHA-256：`eecba1169457a37da10d7cfe5dcbedda4b9f24ef6697d6e59786f474393f0049`。

本机 ad-hoc 签名，未做 Developer ID 公证。镜像 CRC 校验通过；只读挂载后确认版本、最低系统、Applications 链接、许可文件、App/两后端/libusb 的 arm64 架构与签名。包内四个二进制 SHA-256 均与最终签名 App 一致，libusb 使用包内 loader_path。挂载已卸载；没有启动 App 或操作硬件。

证据位于 `build/reports/iteration-476-migration-gates.log`、`iteration-476-contract-diff.log`、`iteration-476-go-exit-audit.log`、`release-3.16-build.log`、`release-3.16-sign.log`、`release-3.16-package.log`、`release-3.16-verify.log` 和 `release-3.16-verification.json`（均相对 Switch 根目录）。发行清单已更新至 `dist/release-manifest.json`。
