# Iteration 611：SIM World VoWiFi 实机探针修复与验收

日期：2026-10-21。工作区：`/Users/StatTrak/Desktop/Switch_MAC/`。

## 发现与修复

实机探针 `scripts/probe-reader-vowifi.py` 使用 `--runtime-root` 创建隔离数据目录，但没有解析相对路径。首次运行时，它把相对 `SWITCH_DATA_DIR` 传给 Rust daemon；后台因此在读卡器连接前退出。失败报告确认没有连接读卡器、启动 VoWiFi、改动 profile、发送短信或拨号，临时运行目录已删除。

探针现在通过 `isolated_runtime_directory()` 将根路径解析成绝对路径，再创建唯一运行目录。新增 `scripts/tests/test_reader_vowifi_probe.py` 覆盖相对根路径，并接入 `scripts/test-migration-gates.sh`。

## 验收结果

- 探针单测：1/1 通过。
- 真机：ESTKme-RED 已枚举并连接；当前卡识别为 SIM World，MCC/MNC `454/03`。
- VoWiFi：状态依次为 `connecting → connected/registering → connected/registered`；最终 `sms_ready=true`、`call_ready=true`。
- 收尾：VoWiFi 停止确认、读卡器断开确认、daemon 正常退出；隔离运行目录已删除。
- 未执行：短信收发、拨号、Profile 切换或下载。能力状态字段不等于真实短信/语音业务验收。

逐项机器报告：[首次路径失败记录](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-611-simworld-reader-vowifi.json)、[修复后实机结果](/Users/StatTrak/Desktop/Switch_MAC/build/reports/iteration-611-simworld-reader-vowifi-r2.json)。本轮使用冷构建 `iteration-610-b11-cold-build-r3` 的 3.46/build 346 app 内 Rust daemon；该 daemon 的 source digest 是 `3aa26e212d3343b41ed5e9992a8ea635071a23bf86819c79c9da67ecdf14be46`。探针脚本随后有代码改动，因此这不是最终冻结源码的发行构建。

## 边界

本次真实注册仅证明当前 SIM World 卡、ESTKme-RED、当前 macOS 网络和该构建下的连接/IMS 注册路径可用。它不覆盖其他运营商、真实短信送达、呼叫音频质量、profile 写入、断线恢复或长时间运行；不据此关闭 P1–P6 验收项，也不计算整体项目百分比。
