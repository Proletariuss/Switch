# Iteration 698：模块网卡准入、磁盘任务退出与连接取消

当前状态：本批定向及完整后端回归通过；来源与产品验收仍开放。未生成 DMG、安装或启动 App，未操作实际 DJI、读卡器、运营商或外部消息服务。P0–P6 未完成，正式台账不重新计分。

## 网卡修正与边界

用户报告仅使用 DJI 模块上网时，关闭网卡提示射频被占用。源码与公共 HTTP 测试确认：自动通话模式检查会跨多个任务持有临时只读租约，原网卡配置请求在这个窗口直接返回 Busy。这个软件缺陷已修正，现场唯一根因尚未经实机确认。

Rust 仅等待 actor gate 确认的临时只读 owner；等待不占 gate 或八个执行槽，原 owner 可以正常释放。一次公共请求共享 25 秒绝对期限，反复核对原 binding、epoch、准入与停止状态。真正业务和未知状态仍拒绝冲突操作。私有一次性标记只在物理派发前产生，已经提交或不确定的 AT 不重发，原请求 ID 回放保持原回执。

Swift 不再先修改按钮状态，也不再固定等三秒暗示设备重新枚举。Store 根据最后观察的配置决定目标，隔离旧诊断；失败保留原观察值，重置后的迟到回执不刷新或提示成功。回执说明设置受理、需要重启，实际连接仍以观察为准。

DJOneHubNative 固定为 `7194ae8cd71da34e0ab3727874dafb82ffd2537b`。其实际 HTTP 接口同样只有 QCFG usbnet 0/1 和 `needs_reboot=true`；另一库函数的重启没有被该 HTTP 接口调用。没有导入或运行 Go 后端。

QCFG 模式写入不是主机即时断网证明。本轮不加入自动重启、PDP 释放或飞行模式。现有产品名/端口匹配不足以授权主机服务禁用，代理链也没有可靠物理出口证明；这些启发式可能误停其他接口或正在运行的 VoWiFi。因此实际主机网卡关闭、配置写后读回和受控生效继续列为待办。固定源码及许可、比较与取舍见本批 `upstream-reference`。

模块与读卡器已经共用 `prepare_card_context_with_stop`、carrier 配置、传输选择与 ordinary IKE/IMS。保留模块 SIM/AKA 和 RF 恢复适配，不复制另一张 SIM 的 ePDG 模板；保持既有代理/fake-IP 路径。详见本批 `module-vowifi-network-reuse-audit.md`。

## 同批稳定性修正

- 全局 archive executor 在创建 future 时登记；job 捕获、输出和观察者三个事实齐备才退休。started panic、销毁与计数不确定保持围栏，诊断锁 poison 不丢实际 owner；最终清理仍可登记。
- S01 通过实际关闭、`dup2` 强制同 fd 复用验证取消归属。pending 分支使用注入 EINPROGRESS 与实际不可写 fd/poll，不冒称自然 pending connect。普通与测试钩子构建分别执行。
- S10-B1 保存在显式 hold 中，未接入此源码。默认关闭的 codec 基础不代表迁移或 GC 完成。

## 验证

冻结 714 项 Rust 输入。初次清单误纳入四项旧 target 生成文件，保留原清单并记录纠正，未改变源码。archive 21、网卡 14、模块控制 149、底层控制 40、公共回执 6 定向全部通过；集合有重叠，不相加。普通后端 check 通过，编译元数据没有 fault/codec 特性。

三组 Swift 通过：本机连接普通/钩子模式、ModuleNetworkRecovery、DeviceCallCorrectness。生产组件配合可控适配器，后者还使用明确提取的生产策略。View/Facade 仅源码审查，不当作整 App 编译。

完整登记 3443 项，相比 697 新增 25 个名称，替换一个旧合并 panic/error 预期，其他原测试保留。完整后端实际单次执行 3435 通过、0 失败、8 忽略，耗时 1836.04 秒；保留忽略项及其独立门禁边界。714 项 Rust 输入前后集合和哈希一致。精确清理无占用 incremental 与 Swift 缓存，回收 3.992458GiB，测试二进制哈希保持不变。部分源码快照不是完整冷构建胶囊，交叉审议不是全项目最终三轮发布审议。

证据目录：`/Users/StatTrak/Desktop/Switch_MAC/build/reports/stability-698/`。
S08 完整恢复链、S10/S11 容量与缓存收口、S12 其他同步边界、S13 冷验收、S14 长稳仍开放；实机/实网、真实音频和外部投递单列未验收。

## 来源工具审议与后续

S13 扫描器现在保守包含已审的 feature/platform 条件模块，any(test,feature) 不作 test-only。首版 57 项单测通过后，独立复核发现旧路径解码 fallback 与 include! 非圆括号两处可能误排源码的边界；旧版实物/函数复现均保存。窄修用严格 Rust source-path 解码并拒绝未审宏分隔符，最终 62 项单测和 18 项独立生产函数探针通过，原 52 项测试名称全部保留。当前源码未使用这些异常路径，不把潜在误排描述为实际丢接口。

当前实际 census 为 306 条 Rust 方法/路径声明、0 Go，含 57 条 HEAD 别名、128 个 Swift HTTP 调用点。门禁仍 exit1：92 项来源人工复核、库存差异及两条已存在 reconcile 路由；未修改 review/inventory 让其变绿。这是声明库存，不能当成整项目完成或运行验收百分比。

用户新提供 `/Users/StatTrak/Downloads/Switch-2.67-source/`，授权只读参考曾正常运行的 N待代码。后续比较记录放在 stability-699，业务实现仍由 Rust 持有；不执行或导入旧 Go 后端。698 证据封存后不再追加。
