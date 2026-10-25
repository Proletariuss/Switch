# Iteration 487：读卡器维护协调与 IKE 证书认证

本轮把普通原生 VoWiFi、旧业务操作和 N待的读卡器占用放入同一准入协调。连接、选择、刷新、切卡及下载使用明确的维护凭据；下载先阻止新会话，再等待原会话及实际卡片任务退出。Starting 已取得的准备占用也能按原任务停止，避免维护任务与原会话互相等待。普通 Go 模块会话与 N待保持原有共存规则。

旧 Go reader Operation/NativeSession 取得独立业务凭据，阻止原生会话在退避期间被另一个卡片操作穿透。Operation 转成 NativeSession 或 N待时原子转移权限；延迟释放不能清除新占用。Go 进程退出仍需等待真实 BLE、PC/SC 和生命周期任务完成。取消、超时或无法确认的结果保留占用，不以未来其他任务的状态充当本次释放证明。明确的 busy 拒绝使用领域错误类型，健康和概览接口不会把这一拒绝绕回旧 Go 路径。

原生 IKE 首轮 EAP 认证现校验服务器 CERT、IDr 和 AUTH：签名绑定实际 SA_INIT 响应、Ni 及 IDr；使用 macOS 系统 IPsec 信任策略校验链、预期名称及签名。预期名按 `ims/ike/remote_certificate_hostname`、旧 `ims/epdg/remote_certificate_hostname`、实际所选 ePDG 的顺序冻结。模板要求证书时必须提供；可选模式收到证书时同样验证。最后的 EAP-MSK AUTH 校验保留。

系统信任工作最多同时两个，取消等待方后仍持有预算直到实际系统调用完成；不额外获取 AIA/OCSP，不改变代理或 DNS 路径。新增薄 Security.framework IPsec policy 桥，复用既有依赖。合成测试证书和密钥已随源码保存并明确标记测试用途。

## 验证及边界

完整 `test-migration-gates.sh` 通过：Rust **2515 passed / 0 failed / 6 ignored**，Go **26 包**测试和 vet、Swift BLE **38 组**、**2726** 条运营商回放及全部协议差分通过。系统信任与本地加密握手 7 项、ordinary 14 项、reader business 20 项的定向测试通过。全部为隔离运行时、合成凭据及本地协议测试，未启动 App、操作实际卡片、发送短信或拨打电话。

RSA-PSS、旧式 ECDSA AUTH 9/10 仍明确拒绝，不宣称已兼容所有运营商。原生生产三个接管门控仍关闭；默认原生接管、N待执行迁移、剩余 Go 退出和实机验收仍待完成。本轮不增加 Go 退出计分，**38.31%** 仅为此前审计的历史估值。

证据：`../build/reports/iteration-487-migration-gates.log`、`iteration-487-business-tests.log`、`iteration-487-cert-system-tests.log` 及同轮专项日志。按用户此次明确要求完成 **3.27 / build 327** 发行包，镜像验证记录位于 `../build/reports/release-3.27/`。

## 3.27 发行结果

[Switch-3.27-macOS27.dmg](/Users/proletarius/Desktop/Switch_MAC/dist/Switch-3.27-macOS27.dmg)，**55,398,327 字节**，SHA-256 `b83216970b56bff0a04b32410008d288adf4b1398fef06cc4b23fe079182c95e`。arm64、macOS 27.0+，本机 ad-hoc 签名、未公证。Release 构建、hdiutil 验证、只读挂载、4 个 Mach-O 的 deep/strict 签名、13 份许可、完整 App 内容/权限/链接与签名构建产物相同；归档副本 SHA-256 相同。编译源码与冻结输入无漂移，构建期间仅完成两份契约审阅元数据，发行后仅更新这四份说明文档。

契约人工审阅及三种检查均通过：341 条契约（声明 Rust 305 / Go 36）、127 个 Swift HTTP 调用点、51 条 HEAD 别名，416 个审阅范围。新增证书测试模块的平台条件整理后，最终 7 项系统信任/握手回归再次通过。报告见 `release-3.27/verification.json`、`gate-summary.json`、`contracts.log` 和 `certificate-final.log`。安装包仍含 Go 后端，不代表全 Rust 重构完成。
