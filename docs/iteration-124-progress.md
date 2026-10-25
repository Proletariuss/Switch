# Iteration 124 — 通话模式 USBCFG 查询迁入 Rust

日期：2026-10-08。

Rust module-radio新增只读 `call_mode_usb_composition_read` typed action，仅接受现有 `CallMode` 或 `Call` reservation，不加载无关飞行模式偏好、不写入或重启。固定 USB/串口 owner 执行 `AT+QCFG="USBCFG"` 并验证唯一响应行、16位 VID/PID、恰好七个0/1功能位；重复行、缺失/额外字段和非法数值均失败关闭。

Go通话模式状态读取、启用前检查、配置写后回读、重启后核验、还原预检与回读、接口就绪判断均已调用该Rust typed action。Go原USBCFG直接查询和文本解析器已删除。仍由Go负责USB配置写入、ADB/helper、恢复状态机与重启编排，并保留现有写后安全比较及重启后完整目标比较。解析/模块拒绝仍呈现为不支持配置，传输失败仍呈现为断连。

验证：Rust daemon **755通过、0失败、3忽略**；Go主包 race 与 `go vet` 通过；Rustfmt、`git diff --check`通过；契约工具42项自测及普通扫描通过，264条method/path契约（Rust184、Go80，30个HEAD派生别名）、127个Swift HTTP调用点、210个审阅范围，契约归属不变。Release `scripts/build.sh` 输出 `/Users/proletarius/Desktop/Switch/dist/Switch.app` 2.86/build286 arm64；`codesign --verify --deep --strict`通过。没有执行实体模块、号卡或写入操作，也未生成DMG。

计入P2B **+0.025计划人周**，由3.875升至3.900/8.0（48.75%）；累计12.370/47.5≈**26.0%**（估算误差约±3个百分点）。此增量只代表USBCFG读取/解析已切换到Rust，不代表USB配置写入、ADB/helper或Call-mode总状态机已从Go退出。
