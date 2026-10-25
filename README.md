<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/AppIcon-dark.png">
    <img alt="Switch" src="docs/AppIcon-light.png" width="128">
  </picture>
</p>

<h1 align="center">Switch</h1>

<p align="center">面向 Apple Silicon Mac 的蜂窝通信与实体 eSIM 卡片工作台</p>

<p align="center">
  <a href="LICENSE"><img alt="License" src="https://img.shields.io/badge/License-PolyForm%20Noncommercial-orange"></a>
  <img alt="macOS" src="https://img.shields.io/badge/macOS-27.0%2B-black?logo=apple&logoColor=white">
  <img alt="Architecture" src="https://img.shields.io/badge/Architecture-Apple%20Silicon-blue">
  <img alt="Backend" src="https://img.shields.io/badge/Backend-Rust-black?logo=rust&logoColor=white">
  <img alt="Version" src="https://img.shields.io/badge/version-3.51-2f81f7">
</p>

Switch 将 USB/QMI 模块、PC/SC 与 BLE 读卡器、实体 eSIM Profile、VoWiFi/IMS、短信和通话能力集中到一个 macOS 应用中。当前版本为 **3.51（build 351，Rust 后端 3.51.0）**。

> 本仓库为私有源码仓库。3.51 是经过软件与发行包检查的快照，不等同于所有硬件、运营商和长期运行场景均已验收。请先阅读下方“验证范围与已知边界”。

`v3.51` 标签继续对应已发布安装包的冻结源码；`main` 已在 2026-10-25 接入最后一次本地开发快照，状态为 `source-not-final-G0B`，用于后续 GitHub PR 迭代，不代表产生了新的 3.51 发行包。两条来源边界见 [SOURCE_PROVENANCE.md](SOURCE_PROVENANCE.md)。

## 下载

| 版本 | 平台 | 安装包 | SHA-256 |
| --- | --- | --- | --- |
| 3.51 / build 351 | Apple Silicon，macOS 27.0+ | [Switch-3.51-macOS27.dmg](https://github.com/Proletariuss/Switch/releases/download/v3.51/Switch-3.51-macOS27.dmg) | `616c276ef75da7965f3a95a9a754d52d4b1701f8802d29ba7cabc2dc0ba5cbde` |

Release 页面：<https://github.com/Proletariuss/Switch/releases/tag/v3.51>

## 安装

1. 下载并打开 DMG。
2. 将 `Switch.app` 拖入“应用程序”。
3. 首次运行时按 macOS 提示授予蓝牙、通知、麦克风等实际使用功能所需权限。
4. 该快照采用本地 ad-hoc 签名，未经过 Apple 公证。若 Gatekeeper 阻止启动，请在“系统设置 → 隐私与安全性”中核对应用来源后选择打开。

全新安装不会预置 Telegram Bot、Bark 或其他转发密钥。覆盖安装沿用用户本机配置和钥匙串；跨版本迁移与回退尚未完成独立验收，升级前请备份重要数据。

## 主要功能

| 模块 | 能力 |
| --- | --- |
| 模块与网络 | DJI 一代/QDC507 USB 模块探测、SIM 与信号信息、QMI/AT 管理、蜂窝网卡与网络顺序、模块 VoWiFi |
| 读卡器与 eSIM | PC/SC、BLE 设备发现与连接、EID/Profile 读取、Profile 管理、B0/B1/9eSIM 适配路径 |
| VoWiFi / IMS | 运营商模板、自定义 ePDG、代理路径、IKE/IPsec 与 IMS 会话管理；配置覆盖不代表对应运营商均已实网通过 |
| 短信中心 | 模块、读卡器与 N待消息归档，发送状态、通知、搜索、删除，以及 Telegram Bot/Bark 可选转发 |
| N待 | 多 Profile 独立短信会话、续期与掉线恢复、状态和运行报告；仅短信，不接入语音，实际并发能力取决于卡片与运营商 |
| 通话 | 拨号、接听、挂断、来电提示、通话记录及 Mac 音频桥接；协商策略为 EVS 优先、AMR-WB 兼容 |
| 诊断 | 网络与设备诊断、AT 控制台、结构化状态和安全事件记录 |

## 支持与接入

- Apple Silicon Mac，最低 macOS 27.0。
- DJI 一代/QDC507 模块；常见 USB 标识为 `2ca3:4006`。
- PC/SC CCID 读卡器，例如 ESTKme 系列。
- 兼容的 BLE 实体 eSIM 卡片，包括项目内已有的 B0/B1 适配路径。
- 可用的实体 SIM/eSIM Profile、相应运营商服务与符合要求的网络出口。

硬件型号相同不代表固件、USB 组合、卡片认证和运营商策略完全一致。涉及写卡、切卡、模块配置恢复或通话模式时，应先备份并确认当前设备身份。

## 架构

```text
SwiftUI 应用
    │  本地 Unix domain socket / typed API
    ▼
switchd-rs（Rust 后端）
    ├── USB AT / QMI / PCSC / BLE 设备 owner
    ├── eSIM、VoWiFi、IMS、短信与通话状态机
    ├── 本地持久化、恢复与任务队列
    └── 内嵌 libusb 与必要的原生 EVS 编解码组件
```

生产包不依赖 Go 后端；Swift 保留 macOS 系统框架和界面桥接。完整迁移与历史验收证据保留在 `docs/`，历史记录不应被当作当前版本的实机通过声明。

## 从源码构建

构建基线为 Apple Silicon、支持 macOS 27 SDK 的 Xcode、XcodeGen、Rust 1.98、Python 3、`pkg-config` 和 arm64 `libusb`。首次构建可能需要访问 Rust registry 和系统包源。

```bash
brew install xcodegen pkg-config libusb
rustup toolchain install 1.98.0

scripts/build.sh                 # 构建 ../dist/Switch.app，不生成 DMG
scripts/launch.sh                # 使用隔离运行目录启动
scripts/quit.sh                  # 正常退出应用与所属后端
scripts/preflight-source.sh      # 可移植的版本、Rust-only 与运营商源契约检查
```

GitHub Workflow 还会构建原生依赖、执行 Rust workspace 与完整 daemon 软件测试、将共享串口状态测试逐例放入独立进程执行，并在 App 相关变更时构建 Release App。免费额度、云端 PR 检查、Dependabot 与权限边界见 [GitHub 自动化策略](docs/GITHUB_AUTOMATION.md)。`scripts/test-migration-gates.sh` 是完整本地验收入口，其中包含冻结的宿主机来源台账；普通克隆不具备全部外部历史证据时，不应把其台账失败解释为业务源码或 DMG 失败。

构建脚本只接受仓库约定的相邻 `build/` 与 `dist/` 工作区布局。发布来源与为私有发布进行的最小转换见 [SOURCE_PROVENANCE.md](SOURCE_PROVENANCE.md)。

## 数据与安全

- 应用数据默认位于 `~/Library/Application Support/Switch`，敏感配置应留在用户本机。
- Telegram Bot 与 Bark 仅在用户主动配置并启用后向相应第三方服务发送所选短信内容。
- 本次发布已排除本机配置文件，并对已识别的真实订阅身份做脱敏；常见凭据模式检查未发现本机 Bot Token 或 Bark 密钥。
- 提交问题前请遮盖 IMSI、ICCID、IMEI、EID、手机号、短信内容、代理凭据和日志中的会话标识。
- 安全报告与披露建议见 [SECURITY.md](SECURITY.md)。

## 验证范围与已知边界

3.51 发行资产已完成：

- DMG 文件系统校验；
- 应用与嵌套组件的严格签名结构校验；
- 版本 3.51、build 351、Rust 后端 3.51.0、arm64 与最低 macOS 27.0 核对；
- 发行包文件清单与常见凭据模式检查；
- 全新安装的短信转发配置为空且默认关闭。

仍不得视为已完成的产品验收包括：所有 USB/PCSC/BLE 实机组合、所有运营商 VoWiFi/IMS 注册、真实短信外部投递、拨打/接听与音频质量、断网/休眠恢复以及长时间稳定运行。具体结果必须以对应设备、卡片、网络和版本的独立验收记录为准。

本 DMG 使用 ad-hoc 签名且未公证，不是面向公开分发的 Developer ID 安装包。

## 完整性校验

```bash
shasum -a 256 Switch-3.51-macOS27.dmg
```

预期输出：

```text
616c276ef75da7965f3a95a9a754d52d4b1701f8802d29ba7cabc2dc0ba5cbde  Switch-3.51-macOS27.dmg
```

## 反馈

请在本私有仓库提交 Issue，并附上版本、Mac 型号、macOS 版本、设备类型和已脱敏日志。涉及卡片或运营商时，请同时说明 MCC/MNC、接入方式和网络路径，但不要提交完整身份号码或密钥。

## 许可证与第三方组件

项目使用 [PolyForm Noncommercial 1.0.0](LICENSE)，仅允许许可证规定的非商业用途。第三方组件、必需声明及各自许可证见 [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) 与 `licenses/`。

EVS 相关源代码/组件的可用性不代表获得任何额外的专利授权；使用者应自行确认适用权利。部分铃声资源的上游来源仍需进一步补齐，因此不得将本私有快照直接改作公开或商业发行。
