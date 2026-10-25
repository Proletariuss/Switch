# Switch 源码来源与同步边界

本私有仓库同时保存两个不同用途的来源边界：不可变的 `v3.51`
发行快照，以及 2026-10-25 最后一次从本地工作目录同步的后续开发基线。
二者不得互相继承验收结论。

## `v3.51` 发行快照

- 规范源码树摘要（SHA-256）：`ed025b3ae498c8d84faf08a633660f1e33c460c1670d2e31f676118a30a14a76`
- 冻结归档 SHA-256：`33208cb0c4ce043c2f5b84f58533847c4ab7d68db25b3852396deac21d748553`
- 冻结清单 SHA-256：`9bf72108aaf3352336c387262bb851574fe84733b2b079be686e87b46898596f`
- 冻结时 Git HEAD：`ed1bf2a55c0aa1aa80b2f84bd0854bd62e656425`
- 冻结树规模：4,078 个文件，249,149,378 字节
- GitHub 标签固定提交：`45c4eae4895d1fba7173bc1f3d0c0ab587cf6d5c`

“规范源码树摘要”和“冻结归档 SHA-256”分别对应目录内容摘要与压缩归档字节，
不应相互比较。已发布 DMG 只绑定本节快照。

## 最后一次本地源码同步

2026-10-25，以 `/Users/StatTrak/Desktop/Switch_MAC/src` 当时的实际工作树为输入，
完成最后一次本地源码同步。输入分支为 `codex/switch-implementation`，HEAD 为
`ed1bf2a55c0aa1aa80b2f84bd0854bd62e656425`；工作树包含尚未提交的后续开发改动，
因此 HEAD 本身不能代表同步内容。

- 最后一次本地同步提交：`9b6a7701f02fe1059a385764ac621d4c87f490c9`
- 净化输入规模：4,129 个文件，249,925,249 字节
- 相对先前 GitHub 源码基线：89 个文件修改、51 个源码文件新增
- 版本字段仍为 3.51 / build 351 / Rust workspace 3.51.0
- 当前开发状态：`source-not-final-G0B`

该提交是“后续只从 GitHub 分支和 Pull Request 演进”的分界点。从此不再把本机
工作目录自动覆盖到仓库。后续修复提交必须以该提交为祖先，并通过 GitHub 托管的
检查记录差异和结果。

## 同步净化与保留项

本地同步明确排除了以下生成或私有运行数据：

- `.git/`
- `build/`、`dist/`、`.build/`
- `rust/target/`、`rust/build/`、`rust/crates/proletariusd/build/`
- `.impeccable/`
- 日志、DMG、profraw、环境文件、私钥、抓包、AppleDouble、Python 缓存及
  `fixtures/private/`

排除项合计约 816 MB，并包含四个运行期 Unix socket。最终同步副本未发现嵌套
Git、符号链接、超过 50 MiB 的单文件，亦未发现标准格式的 GitHub、Telegram、
Bark 或 AWS 用户凭据。

以下发布层文件没有被旧本地副本回退：

1. `.github/**` 的 GitHub 托管自动化、Dependabot 与 PR 模板；
2. 扩展了 AppleDouble 和生成物排除规则的 `.gitignore`；
3. `docs/VOWIFI_MIGRATION.md` 的真实 IMSI/IMEI 脱敏副本；
4. GitHub 下载、校验、自动化和验收边界版 README；
5. `SECURITY.md` 与本来源说明；
6. `native_serial_module_supervision_tests.rs` 中已经在 GitHub Runner 验证的确定性
   路由屏障、直卡准入屏障和专用短信 worker 收束修复；这些仅影响 `cfg(test)`。

两份 `.test-pem` 是目录内明确声明的公开合成测试签名钥，不是用户证书或生产私钥。
历史证据中的绝对本机路径按私有仓库来源记录保留，不应在其他机器上直接使用。

## GitHub 后续迭代

从最后一次本地同步起至 2027-10-25，自动化按
[`docs/GITHUB_AUTOMATION.md`](docs/GITHUB_AUTOMATION.md) 执行：

- 所有 PR 在 GitHub 托管 Ubuntu Runner 上执行源码契约、格式和 workspace 元数据检查；
- 生产代码变化在 GitHub 托管 macOS Runner 上执行 Rust 软件门禁；
- App 相关变化才追加 `Switch.app` 构建，不在 PR 中生成 DMG；
- RustSec 和 Dependabot 定期检查依赖，并且只提出 Issue 或 PR；
- 禁止自动合并，禁止工作流直接改写 `main`。

无付费 Copilot，因此 GitHub 不会进行语义性 AI 自动改写。代码级修复由审议结果形成
独立分支和 PR，并由 GitHub Runner 复验。

## 构建可重现性边界

- 基线平台：Apple Silicon、macOS 27、支持 macOS 27 SDK 的 Xcode。
- Rust 工具链：1.98，依赖由各自 `Cargo.lock` 固定。
- 原生依赖包括 XcodeGen、Python 3、`pkg-config`、arm64 `libusb` 和项目内 EVS 组件。
- XcodeGen、Homebrew `libusb` 及部分宿主工具未使用完全密封的二进制来源，因此当前
  是“有来源和锁文件的可复建快照”，不是位级可重现构建。

## 发行资产边界

- DMG：`Switch-3.51-macOS27.dmg`
- 大小：55,967,521 字节
- SHA-256：`616c276ef75da7965f3a95a9a754d52d4b1701f8802d29ba7cabc2dc0ba5cbde`
- 签名：ad-hoc，未 Apple 公证

DMG 是 `v3.51` 冻结时已验证的既有资产；最后一次本地源码同步和其后的 GitHub
迭代均未重新构建、替换或扩大该 DMG 的验收范围。
