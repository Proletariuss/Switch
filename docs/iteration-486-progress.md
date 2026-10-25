# Iteration 486：普通 VoWiFi 原子准入与实际退出确认

Rust 统一协调原生读卡器、Go 读卡器及模块的普通 VoWiFi 启动占用，模块先取得准入再进行 AT/RF 准备。恢复、退避及 IKE 重认证沿用同一占用；取消或超时不代表资源释放。Go 与原生任务真实退出、清理完成且同一代次释放回执确认后，才允许后续启动。清理状态未知时保留 starting/retiring 占用，避免重复启动与无限累积清理任务。N待保持原有多会话行为。

Go 进程退出等待错误不再被忽略；只有实际取得退出结果才能退休旧 producer。私有准入接口需要内部令牌，公开入口固定返回 404；正文 4 KiB、读取限时 5 秒，清理请求使用保留预算。失败发布时回滚未实际启动的 worker 引用，避免注册表拒绝后留下无法释放的占用。

运营商 IKE local_identifier 与 EAP 身份分别解析；完整用户模板仍沿用既有 ISIM 访问身份覆盖语义。加密网络回归验证最终 AUTH 绑定实际发送的 IDi。保留自定义 ePDG/端口与既有代理路径。

完整离线门禁：Rust workspace **2472 passed / 0 failed / 6 ignored**（daemon 1772/0/3），Go 全包 test/vet、Swift BLE 38组、2726项 carrier replay及所有协议比对通过。最后的 Go 发布失败修复另经定向 race 和全包 test/vet 验证。共享准入 Rust 7项、原生 service 29项、新 Go admission 11项回归通过；未操作真实号卡、发送短信或拨打电话。

契约人工审阅与普通一致性检查通过：341 条契约（声明 Rust 305 / Go 36）、127 个 Swift HTTP 调用点、51 条 HEAD 别名、407 个审阅范围。只增加一个私有接口，不将此作为默认原生业务接管或 Go 退出计分。

三个原生生产接管门控仍关闭。证书链与 IKE 签名验证、读卡器维护和会话启动协调、默认原生接管、N待接管及实机验收仍待完成。38.31%（18.195/47.5）只保留为此前审计的历史估值，不能视作本轮重新测量的完成率。

## 按用户要求发行 3.26

发行 **3.26 / build 326 / arm64 / macOS 27.0+**：[Switch-3.26-macOS27.dmg](/Users/proletarius/Desktop/Switch_MAC/dist/Switch-3.26-macOS27.dmg)。大小 55365291 字节，SHA-256 `e2430a322f8e8926da4c2d5470deff38071161edc6505a24e84db6941bc59fed`。

Release 构建、hdiutil 校验、只读挂载、codesign deep/strict、全部 4 个 Mach-O 签名与 13 份许可文件检查通过。镜像内全部文件内容、权限及链接与签名后的 dist/Switch.app 相同；archive 副本校验相同。构建输入无漂移，最后 Go 错误分支修复已重新构建。本机 ad-hoc 签名，未公证；未安装、启动 App 或做实机业务验收。

证据位于 `../build/reports/release-3.26/`（build.log、contracts.log、verification.json），完整回归日志为 `../build/reports/iteration-486-migration-gates.log`，最后 Go 回归为 `iteration-486-go-full-tests-final.log`、`iteration-486-go-vet-final.log` 和 `iteration-486-go-publication-race.log`。
