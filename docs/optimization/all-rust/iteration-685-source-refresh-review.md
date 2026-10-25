# Iteration 685 — 契约指纹与 3.49/349 来源复核

i684 隔离冷胶囊在 HTTP 契约审阅与清单的旧源码指纹处失败，未产出可验收 App。i685 对 `main.rs` 公开 `/api/internal` 与私有 `.in` 边界做了逐项手工契约复核，并更新契约审阅和清单中的当前精确指纹。304 个接口的 ID、方法、路径与 Rust owner 均未增删或转移。此项修复的是来源证据漂移，不授予运行时或产品 PASS。

待验候选版本统一为 **3.49/build 349**：`VERSION`、`BUILD_NUMBER`、`project.yml`、Rust 工作区清单与锁文件已一致。`Switch.xcodeproj/project.pbxproj` 是由 `project.yml` 生成的工程文件，不在 `source-identity` 或隔离冷源码清单内，后续构建须核对 App 实际版本和构建号。相对不可变 i684 的 547 条选择性清单，7 个已选文件变化，契约审阅及清单两份既有文件首次入选，共 549 条；选择性指纹不等于冷构建的完整源码清单。

QMI 九个源文件与 57 个精确锚点未改，私有鉴权、操作与清理链的静态结论不变。正式冷验收仍要求 `audit-clean-build` prepare、execute、verify 的同源码完整收据，之后再审议可用 App、升级/回退及适用的设备/运营商项目。i684 失败记录原样保留，不能将这次元数据修复解释为冷构建通过。正式台账维持 49 父项、115 叶、21 场景类、0 PASS / 5 PARTIAL / 110 NOT_REVIEWED；整体分母未冻结，P0–P6 尚未完成，最终验收前不生成 DMG。
