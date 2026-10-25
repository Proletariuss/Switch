# Switch 3.40（构建 340）

2026-10-14。按用户明确要求，以 Iteration 501 已冻结源码生成。

[下载 DMG](/Users/proletarius/Desktop/Switch_MAC/dist/Switch-3.40-macOS27.dmg)。Apple Silicon，macOS 27.0+。

- USB eSIM 概览、刷新、健康读取和最近成功快照迁入 Rust；缓存绑定物理连接及修订号，写入前失效，取消页面请求不会提前释放实际读卡工作。
- 清理已失去入口的旧 Go IMS/通话/会话执行代码；维护操作依据精确原生退出证明，仍保留非 USB、蜂窝和设备发现兼容能力。
- 完整软件门禁、Release 构建、严格签名、镜像校验通过；只读挂载确认来自此 DMG，包内应用与已签名构建逐文件一致，4个 Mach-O 均为 arm64，13份许可证齐全。最低系统版本和动态依赖实际解析通过；构建至最终验证源码零漂移。

本机 ad-hoc 签名，未公证。没有安装替换现有应用；没有开展本版实卡、运营商、真人音频或长稳验收。仍包含 Go 后端，全 Rust 目标未完成。

SHA-256：`9935ea0be55766698472a66b3bf144c864c534309188e51feca5cdeadfd80262`

大小：52,160,782 字节。

[本轮源码记录](iteration-501-progress.md)。证据：../build/reports/release-3.40/。验证后仅更新发行文档、README和项目进度记录。
