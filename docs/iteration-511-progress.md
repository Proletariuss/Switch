# Iteration 511：旧 Go 源码、模块与测试工具链退出

2026-10-16。源码树与默认构建、测试、运行链现均不依赖 Go。SwiftUI、CoreBluetooth/音频等必要系统桥及原生 EVS/AMR-WB C codec 保留。本轮完成软件退出门槛，不代表设备、运营商或全项目验收完成。

## 迁移与删除

- 删除旧 backend 目录及 Go AST 扫描工具，共3326个文件，其中861个Go文件（495个业务/库文件、365个测试文件及1个扫描工具）、10个Go模块/锁文件。源目录已逐项确认不再含Go源码和模块文件。完整旧目录先归档到源码树外的build/reports，并逐文件复核SHA-256；归档不参与当前编译和测试。
- 1967份原生codec、运营商源数据、证书、元数据、部署清单及许可资源按原字节迁出；另保留8份历史BPP/SBPP样本。原有3份EVS及2份PKI数据已有完全一致的Rust副本。无当前消费者的OpenCORE源码/预编译文件随旧目录退出，来源许可保留。保留历史Go名称的许可或JSON出处不是Go工具链依赖。
- 旧Go独立生成的5组6466项投影/运营商结果、10组297次协议调用结果冻结为带来源、SHA、数量及负例校验的数据。日常测试读取固定预期，不再运行Go或用待测Rust重新生成预期。SDP组仅保留明确准入预期，未将过去Go调用Rust的适配检查包装成独立协商证据。PCMA/AMR-NB历史样本不启用这些通话编码。
- 运营商生成器由Rust直接读取1540份YAML，完整保留855个selector、685个profile，重建2726组选择数据及3个拒绝结果；全字段与独立旧结果一致，输出排序确定。载入使用重建数据，不复制内嵌JSON。
- 新增真实独立进程的转发队列锁互斥/退出释放测试，保留SWF1格式；补齐AMR-WB音频黄金向量、OMA-CP/WSP样本及历史下载包分段测试。BPP测试经过真实原生加载入口和内存卡传输，在两种APDU大小下逐段比对完整字节。
- 默认及可选门禁移除Go调用；mise/CI与workspace环境不再选择或准备Go。退出检查覆盖整个源码树、模块清单、默认命令、CI和工具版本配置；缺失或无法验证的输入不能当作退出成功。

## 已完成的软件验证

| 验证 | 结果 |
| --- | --- |
| 完整Rust workspace | 2955通过、0失败、1忽略；daemon2198通过、0忽略 |
| 单独Swift/Rust转发联动 | 8项Rust测试及Swift双向字段比较通过，覆盖上述唯一忽略项 |
| 检查工具负例 | Go退出18、契约50、固定样本完整性8，全部通过 |
| Swift BLE | 物理完成凭据模拟及39组manager事件序列通过，无真实蓝牙卡操作 |
| 固定协议回放 | 297次通过；6466项投影/运营商结果由默认Rust测试逐项验证 |
| 原生运营商导出 | 黄金测试与registry/cases两条CLI均通过 |
| 历史BPP分段 | 4组样本及7段sequenceOf87回归通过，原始样本未改写 |
| 接口契约 | 294/294由Rust接管；127个Swift HTTP调用点、54条HEAD别名不变 |
| 审查更新 | 仅3个已审查范围更新指纹、59个已删除Go范围移除，其余422个不变，共425个 |
| Release应用 | scripts/build.sh通过，包内仅switchd-rs与libusb，严格签名及包内相对依赖通过 |
| 包内后端 | 使用独立数据目录、不存在的显式串口启动，原生状态、无Gosocket、SIGTERM退出0及socket清理通过 |
| 源码一致性 | 2846个冻结构建/测试输入零漂移；完整门禁、应用构建及联动测试均未触发Go命令陷阱 |

BPP新增测试的首次失败来自测试加载器对历史Base64全文采用了较严格的读取方式。修复只对齐旧单TLV读取边界，并明确校验样本2/4各自多出的两个填充字符；没有改生产代码、原始样本或期望分段。首次失败日志与修复说明均保留。

本轮沿用现有Rust/native/Swift缓存，不能据此声称已完成全新机器冷构建、三次冷热性能测量或实机业务。应用版本仍为本地3.42/build342；既有DMG没有重打包，不包含本轮更新。

## 未完成与下一步

继续修复退出流程：一次清理失败不应跳过其他独立资源；未知物理释放状态必须保留依赖屏障，不能盲目关闭传输；清理监听错误不能丢弃尚未完成的资源持有任务；关闭日志管道不能导致panic。已完成独立审查和具体修复计划，尚未在本轮实现。

当前12项业务/设备验收标记仍未通过，cutover readiness保持false。字段中的40%仅为粗粒度组件计数，不是项目完成率；31.4%/38.31%均为历史账本，不作为当前精确百分比。正常SM-DP+下载、支持硬件/卡片、多运营商、双向短信及转发、EVS→AMR-WB通话与音质、升级恢复、性能和自然到期长稳仍须逐项验收。9eSIM6982保持排除。项目目标保持active。

本轮未生成DMG、未替换用户已安装应用、未执行实体卡变更或对外短信/拨号。

证据：[完整汇总](../../build/reports/iteration-511-gate-summary.json)、[源码退出](../../build/reports/iteration-511-source-retirement.json)、[完整门禁](../../build/reports/iteration-511-migration-gates.log)、[审查差异](../../build/reports/iteration-511-final-review-delta.json)、[应用验证](../../build/reports/iteration-511-app-verification.json)、[包内后端验证](../../build/reports/iteration-511-packaged-runtime-smoke.json)、[BPP修复说明](../../build/reports/iteration-511-bpp-loader-repair.md)、[退出流程修复计划](../../build/reports/iteration-512-native-shutdown-repair-plan.md)。
