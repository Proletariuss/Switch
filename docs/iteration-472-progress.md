# Iteration 472：通话音频 readiness 完全由 Rust 读取

日期：2026-10-13。移除 `POST /api/internal/call-mode/audio/readiness` Go 适配器。Rust 的通话音频启动现在直接通过 Rust module-radio owner 读取 USB composition 与 IMS/VoLTE 配置，使用 Rust ADB 探测器、Rust runtime 文件校验和模块部署代次验证完成 readiness；USB 接口未就绪时维持旧顺序，不提前查询 voice facts。reservation 共享以及跨取消持有的通话模式文件锁仍覆盖 readiness 与音频路由执行。

Rust 新增 readiness facts 的 reservation 借用/创建与释放逻辑、USB 未就绪短路及完整事实读取测试；音频启动的阶段顺序和拒绝映射有单测。删掉 Go route registration、readiness adapter 与对应测试，Rust ownership allowlist 同步移除该内部路由。此切片接管了通话音频启动的 readiness 子流程，不代表 call-mode USB/IMS 写入、呼叫信令、设备系统代理或运营商通话已全 Rust 化。

离线验证：完整 `scripts/test-migration-gates.sh` 通过，Rust workspace **1,480 passed / 0 failed / 3 ignored**；Go 全仓 test/vet、Swift BLE、2,726 项 carrier replay、USB/radio、HTTP method+HEAD、IMS/SDP/AMR/MESSAGE/USSD/dial-target/rejection parity 均通过。合同 census `354 method/path (Rust 295 / Go 59, 含 51 HEAD aliases)、127 Swift HTTP calls、382 review scopes`；合同 `--diff`、Rustfmt 与 `git diff --check` 通过。门禁 SHA-256：`981ae59b3c9c12787d312cff530bedc931993e07f9e4b50cae5abee49b300c17`；合同差异 SHA-256：`f48a1047183edd150b906840139160b0a1cf8d82103e61fec6807eb8a1bcd438`。结构退出审计仍显示 6 类 Go build/runtime/source 阻塞、266 个生产 Go 文件；详见 [退出审计](../build/reports/iteration-472-go-exit-audit.log)。

按现有子工作包核算，将 readiness 读取/门禁从 Go 搬入 Rust 计入 P5 **+0.025 人周**；累计 **18.095 / 47.5 = 38.09%**，估算误差约 ±3 个百分点。未访问读卡器、SIM、短信、电话或运营商网络，因此此处只证明离线行为与契约，不证明硬件或运营商验收。9eSIM `6982` 仍排除。

按用户本轮要求从当前源码构建并核验 [Switch-3.15-macOS27-iteration472-20261013.dmg](../dist/Switch-3.15-macOS27-iteration472-20261013.dmg)：`hdiutil verify`、包内 deep/strict code signature、版本 3.15/build315、arm64 核查通过；SHA-256 `4caaee840d10b5441adb1fcb0949fc808032ddb1907b6b25b9d37e6ebeb2900e`。使用本地 ad-hoc 签名，未做公证。该包仍嵌入并运行 Go backend，不能视为全 Rust 交付。
