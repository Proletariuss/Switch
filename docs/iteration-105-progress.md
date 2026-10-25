# Iteration 105 — 读卡器 IMEI 配置归 Rust

日期：2026-10-08。继续执行[全 Rust 后端改造目标](optimization/2026-10-04-all-rust-backend-plan.md)，所有修改均在 Switch 工作区。未连接读卡器或号卡，未执行 VoWiFi、通话或短信操作；未生成 DMG。9eSIM `6982` 仍排除。

## 已完成

- Rust 接管 `/api/reader/imei` 的 GET、PUT、DELETE handler 与持久化；Go mux 和旧 handler 已移除。公开 GET 的 `imei/config`、PUT 的 `imei/message`、DELETE 的 `message` 字段及既有成功文案保持兼容；HEAD 仍映射 GET。
- 继续使用原数据根目录下的 `imei.json` 与 `{"imei":"..."}` 格式，PUT 接受空值或去空格后恰为 15 位 ASCII 数字。持久化使用与 Go 兼容读者共用的 `imei.json.lock`、0600 文件权限、0700 数据目录和原子替换；拒绝符号链接、超大输入及非普通文件。GET 不创建文件。
- Go 仍是当前 VoWiFi 会话执行者，因此不删除它的读取适配；每次需要读卡器 IMEI 时会重新读取 Rust 写入的共享文件，并在空配置时沿用旧随机身份生成逻辑，避免 Go 长驻进程继续消费启动时的缓存值。
- 持久写入失败现在返回 HTTP 500 和明确错误。旧 Go handler 在磁盘写入失败时仍可能返回成功；本次选择报告真实持久化结果，避免 UI 显示已保存但重启后丢失。正常成功响应形状不变。
- 所有方法的 405 `Allow` 按字典序固定为 `DELETE, GET, HEAD, PUT`，与通用 HTTP 策略保持一致。

## 验证

- Rust daemon 全量：**694 passed、0 failed、3 ignored**；覆盖响应形状、输入裁剪/校验、尾随 JSON 兼容、文件权限、并发写入、符号链接拒绝和 GET 无副作用。定向验证的排序策略回归通过。
- Go `go test ./cmd/proletarius-macos -count=1`：通过，包含 Go 运行期间重读 Rust 配置以及清空后沿用身份生成逻辑的兼容测试。
- 迁移契约检查器自测：**41 项通过**。已刷新契约清单并通过来源审阅指纹检查：**255** 条 method/path（Rust **164**、Go **91**，含 **24** 条 HEAD 别名）、**198** 个审阅范围、**127** 个 Swift HTTP 调用点。HEAD 只是派生兼容方法，不另计迁移收益；Rust-only 门禁仍因 Go fallback 与未迁移路由/依赖而不通过。
- `scripts/build.sh` Release App 构建与 arm64 签名验证通过，输出 `/Users/proletarius/Desktop/Switch/dist/Switch.app`。Rustfmt 未安装于当前 toolchain，`cargo fmt --check` 无法执行；未安装全局工具。`git diff --check` 通过。
- 未进行硬件或运营商验证；本变更只迁移配置接口与共享文件 owner，不代表读卡器业务、IMEI 相关 VoWiFi、IMS 或后端已全面 Rust 化。

## 进度与后续

仅将这个有界生产配置 API、持久写入 owner 和 Go 活跃读者兼容接线计入 P2B：**47%→47.5%**，增加 **0.04 计划人周**。原 47.5 人周中值基线由 11.825 调整为 **11.865 / 47.5 = 25.0%**（估算误差仍约 ±3 个百分点）；路由数量不直接折算进度。剩余硬件业务、BLE、读卡器卡片操作、IMS/VoWiFi、N 待、短信设备收发、语音、eSIM/LPA 全链及 Go-free/实机验收仍未完成。中间不生成 DMG。
