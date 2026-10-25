# Iteration 202：普通 USB 模块 eSIM 总览读取迁入 Rust

日期：2026-10-09。范围限定在 `/Users/proletarius/Desktop/Switch`。

普通 USB 绑定下的模块 eSIM 总览请求现在由 Rust 通过共享 `module_radio` eSIM reservation 和完整 `usb_at` transaction owner 读取。新增的 Rust `ModuleEuiccChannel` 将 `AT+CCHO/CGLA/CCHC` 适配到现有 `proletarius-euicc` ES10c/APDU 实现；读取 EID、eUICC 信息、配置地址和 Profile 列表，并保留 Profile 组级读取失败。逻辑通道在释放 USB transaction 前显式关闭；清理失败不能被可选产品元数据读取吞掉。物理 eUICC 不可访问继续返回既有错误分类，不用空 Profile 列表伪装成功。

显式 `refresh=true`、VoWiFi 占用 SIM 通道、非 USB 绑定与完整 eSIM 写操作尚保留现有兼容路径。Rust读取失败时，只请求 Go manager 的只读 `cached_only` 快照；该请求不启动硬件读取，只有已知 EID 的 last-good snapshot 才以只读能力返回。这样避免 APDU 失败后由兼容回退再次读卡。9eSIM `6982`不在本次范围内。

定向回归验证有效 EID 与 Profile 组级部分失败、所有 ISD-R AID 均被拒、卡片读取失败后关闭逻辑通道、产品元数据错误不隐藏通道清理失败，以及 AT/APDU 不重试边界。验证命令 `scripts/test-migration-gates.sh` 通过：`proletariusd` 1,105 passed / 3 ignored，Rust workspace/doc tests、Go 全仓 test/vet、2,722 项载波回放以及 USB/radio/HTTP method+HEAD/SDP/AMR parity 均通过。单独 Go eSIM adapter tests 也通过。Rust 编译仍有此前 `native_vowifi_session.rs` 两条 dead-code warnings。

保守计入 eSIM/LPA **+0.025 计划人周等值**：1.125/5.5 调整为 **1.150/5.5（20.91%）**；累计由14.770调整为 **14.795/47.5（31.1%）**，整体百分比按一位小数未变化。USB eSIM 实机读取、Profile 启停/改名/删除、刷新与下载、Go 适配器退出、长期运行和零 Go 构建/运行仍未验收；未连接或操作实体卡、未下载 Profile、未发送短信/拨号、未生成 DMG。
