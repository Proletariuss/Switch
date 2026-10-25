# Iteration 099 — 3 HK / SIMworld VoWiFi 模板兼容

日期：2026-10-07。用户要求按提供的 `3_hk.json` 调整，并明确要求完成后输出 DMG。全 Rust 后端仍为长期目标，本轮兼容修复不视为业务迁移完成。

## 实现

用户模板原样嵌入，只作用于归属 454/03 且命中通用 Hutchison HK 的卡片；专有 MVNO 与其它 PLMN 保留原设置。Rust 运行时返回完整模板，Go 执行层在新会话中应用，并保持显式配置覆盖优先级。

根据用户确认的 VoHive 来源，核实了 `eap_direct`、ISIM、COOKIE 编码及 CP 开关的真实含义。独立实现初始 REGISTER 复用本次已验证 EAP RES，避免为该初始请求再次调用卡片 AKA。新增会话凭据快照有独立副本、代次隔离和清理；本模式请求完整 EAP，不使用缺少新凭据的快速重认证或旧恢复票据。

完整 ISIM 的私有/公开身份与域同时用于 IKE、EAP、IMS，并明确选择 ISIM 应用鉴权；无 ISIM 保留 USIM 路径，部分配置和不支持的 ISIM 操作明确失败。模块、普通读卡器、N待三条路径均接线，原读卡器代次与卡片身份约束保留。

补齐模板所需的 PFS14 CHILD_SA 换钥、完整算法列表、IPv4、两小时重认证/注册配置、IMS 头部与 SMS Contact。换钥先验证回复与 DH/算法/流量选择器，再发布新 SA。修正 REKEY_SA 与旧 SA DELETE 使用本端入站 SPI。首包 CP 和模板特定 COOKIE 编码均按来源行为处理；模板数据未提供的降低认证校验选项不引入。

详细字段与来源：[模板兼容记录](optimization/2026-10-07-three-hk-template.md)。没有执行外部安装脚本或二进制，没有连接真实运营商或操作卡片。

## 验证与交付

完整 Rust workspace：**1138 通过、0 失败、14 默认忽略**。Go `vet ./...` 与完整 `go test -race -count=1 -json ./...` 通过：**1415 个顶层 / 2496 个含子测试通过、0 失败、17 默认跳过**，26 个有测试包通过。跳过项需要专用跨语言、进程或测量夹具；本轮未将其全部视为执行过。运营商 Rust→Go 的 2722 项回放已单独执行通过。SWu 独立模块完整 race 通过。

新增回归覆盖真实 REGISTER 固定二进制 RES 向量、缺材料拒绝、没有重复 SIM AKA、ISIM 完整/缺失/部分字段与覆盖优先级、原生 EAP 的严格应用选择、COOKIE 字节链、完整 PFS14 加密换钥及非法回复保护。N待的 4 个合成场景真实经过 broker、PC/SC RPC 适配、ICCID/IMSI 核对及 USIM/ISIM AUTHENTICATE 选 AID，每次保持单一事务；没有打开实体读卡器。

契约审阅和一致性检查通过：**248 项（Rust 150 / Go 98，含 20 个 HEAD 别名）、127 个 Swift 调用点、185 个审阅 scope**。更新了 5 处已经交叉审阅的源码指纹，未增加路由或冒称 Go 退出。`git diff --check` 通过。

主要证据位于 `Switch/build/optimization/`：

- `2026-10-07-threehk-rust-workspace.log`
- `2026-10-07-three-hk-carrier-replay.log`
- `2026-10-07-threehk-go-vet.log`、`2026-10-07-threehk-go-full-race.jsonl`
- `2026-10-07-threehk-swu-direct-race.log`
- `2026-10-07-threehk-multi-isim.log`
- `2026-10-07-threehk-contract-{review,write,check}.log`

交付 `Switch/dist/Switch-2.82-arm64.dmg`，**50,000,316 字节**（约 50 MB），Apple Silicon，最低 macOS 27.0。Release 构建成功；Swift App、Go 兼容后端、Rust 后端及 libusb 均为 arm64，严格签名检查通过，沿用 ad-hoc 签名，未公证。仅同步 22 个 workspace 自有包版本为 2.82.0，未升级第三方依赖。

`hdiutil verify` 通过。只读挂载后，26 个包内文件/链接的内容、权限与已签名 App 完全一致，版本 2.82 / 282、Applications 链接与相对 libusb 加载路径均已验证；已卸载镜像。两个后端二进制均包含逐字节相同的用户 JSON。未启动 App、未替换系统中已安装的 App。

SHA-256：`895864235789cae08e72390cf628fe8b2650d75550a4aaf2e1b302e3b745b695`。

构建/打包日志为 `2026-10-07-switch-2.82-*.log`，完整包清单与只读挂载验证为 `2026-10-07-threehk-release/{app-verification,dmg-verification,embedded-template-check}.json`。

## 项目进度

全 Rust 后端的原 47.5 人周基线保守已验收估算维持 **24.9%（11.825 / 47.5，约 ±3 个百分点）**。本轮是新增用户模板兼容；不能用测试数量或配置字段数增加已迁移比例。Go 仍负责部分生产业务，纯 Rust VoWiFi 会话尚未对外启用，实际 SIMworld 注册与长期稳定性未验收。EVS 优先、AMR 回退、不使用 PCMA 的目标保留；9eSIM `6982` 继续排除。
