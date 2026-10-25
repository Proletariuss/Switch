# Iteration588：原owner语音supervisor与正常安全STOP前置

2026-10-19。整体仍为 **P1–P4同步验收、P5完善、P6未完成**，P0行为验收亦未完成。
接口接管 **100%（294/294，含54条继承HEAD）**，458个审阅scope；这不是全项目完成率。
115个候选叶子和49个不计分父分类仍不是冻结分母，本轮不因整理证据新增产品PASS。

## 实际产品改动与边界

正常Start使用源码匹配的ARM Rust supervisor。本地控制记录在实际RootProof held目录耐久提交，早于本次managed remote目录/key部署、serve和helper launch；
但旧六项资源及ARM supervisor上传、prepare_module_voice_audio可能已经发生，不宣称该记录早于所有远程副作用。
supervisor保留真正Child，单owner串行TERM与wait/reap，终态后不再signal旧PID；SIGCHLD继承状态和Linux parent-death安装竞态有明确处理。
认证协议使用独立HMAC域、原session和新nonce，完整有界LF记录及真实EOF均必需。密钥不进入argv或日志。

STOP等待认证child终态后，执行原DSP回滚、UAC0和两PCM明确closed读回；实际USB transition释放确认后才RELEASE。
成功managed控制、Start/Stop/Status及capture有checked interface release；认证RELEASE和最终checked ADB release/drop均确认后，才耐久写入正常完成receipt并retire原义务。
旧部署及failed-preparation rollback仍使用原legacy with_usb/Drop，不宣称所有ADB路径均已checked。失败prepare没有completion，也不retire义务。
取消、超时、错误签名、释放失败或ACK未知保留owner及证据；无retained owner的Stop不事后收养旧PID。
原PID/start/argv/boot/route观察本身不是kernel3.18安全kill权限，新进程空registry也不证明历史clean。

完成receipt只豁免自己的voice-control启动槽位，其他backup、legacy或journal原因仍隔离。
完整写入后sync失败可凭私有closed事实仅本地重试原receipt；部分final文件保持Unknown，不覆盖、不修复、不retire。
这是正常原owner安全STOP前置，**不是独立physical RecoveryRight、native恢复worker/STOP/restore、独立恢复journal/receipt或唯一Gate解封完成**。

## 真实软件与包内验证

- 最终cut `final-v5`：冻结2972项输入，相对587有65项改变或新增，其中43项新增、零移除；新增数已包含在65中，含许可证及构建输入。
- 主软件证据复用实际`final-v3`：完整daemon **2880通过、0失败、2个进程helper不算单测通过**；比587净增22项、用例无删除，定向228项通过。v5没有重新编译或重跑这些原daemon/API/FS/supervisor套件；精确snapshot对照证明v3→v5只变scripts/build-evs.py，其他输入均未变，复用单独记录。
- 为新增干净archive，v5另外实际重新编译并完整运行call/codec套件 **70通过**，直接绑定新archive `961c7bcaf6230b6d6bf26202970d3aea54b6e29a918eaca275bbedd6f9454486`；该新增验证不计入daemon2880、net+22或v3复用声明。之后实际Release10向量也绑定该新archive。
- platform-fs 8项、API常规9项加独立DTO专项1项、native集成10项、另列supervisor host13项（library12+实际CLI1；binary零用例不加分）。
- 真实Swift文件→Rust DTO→daemon重启3场景8进程；journal死亡/重启8阶段16进程；实际main metadata诊断2场景2进程；包内生命周期17场景23进程、三种日志管道及10种Release向量通过。
- GUI/Swift使用源码未变且指纹核验的581产物，四个Swift程序在v3实际运行；v5为handoff仅重新复制同指纹工具，没有新Swift/GUI编译。额外call测试有实际新编译，原daemon套件没有重编。v5实际再次运行Release backend recipe，是否有Rust daemon重新编译按实际日志单列，不因recipe运行就假称重新编译；arm64包和ad-hoc严格签名核验通过。后端SHA `522fe4ab958e8a87946c81389fcef35a5e3329d02535847539ab1c3bfb96cd26`。
- ARM supervisor `d8f5c499828879d3c71f08bc4172ae54fc721b44a69753b95802b3eafa199af4` 实际静态编译和ELF32/EABI5 soft-float、无动态loader检查通过，并核对嵌入一次及完整license payload；没有在真实ARM模块执行。
- ARM资源第二次相同构建保持bytes/inode/mtime。最终EVS builder使用可重建的archive-manifest缓存，key绑定有序实际object names/bytes SHA、实际libtool路径/bytes SHA、原static args，原archive实际SHA也必须匹配才命中并跳过libtool；miss仍真实临时归档、完整bytecompare、必要原子发布与sync。codec源码及编译flags未改变。
- EVS实际初始化后再两次验证命中，archive和manifest两文件bytes/inode/mtime保持。之后v5正常backend增量recipe实际通过，ARM/EVS资源及候选字节保持、无Rust重新编译记录、license目录不嵌套。这个受信任串行workspace构建缓存不是生产恢复凭据，也不代表整机性能验收。

## 保留失败与验收限制

所有原ARM/host preflight失败、日志、诊断与基线保留。XNU fully-shutdown后修改timeout的EINVAL通过有界poll/非阻塞I/O处理，未忽略任意错误或放宽EOF/验签。
手动review的after原本是normalized token fingerprint，另列raw_source_sha256才是文件原bytes hash；曾误读两种hash域的reconciliation已保留为观察，不代表原manual缺审阅或曾stale。
v1完整daemon2880通过是历史cut；v2完整daemon2879通过、1失败和software=false原样保留。失败来自rekey夹具未声明本association零AUTH_LIFETIME后的合法EOF竞态；v3仅修该test fixture，保留真实DELETE、2秒和全部重认证/AKA/REGISTER/SMS/call断言，并实际重跑通过；v5复用这一软件门。
原v3 backend增量构建timeout120、forced=true、group_gone=false的失败报告仍为iteration-588-incremental-build-result.json，事后确认原58316组已全消失且没有追加signal。无条件libtool重写EVS archive触发后续Rust编译，后续新的成功增量报告另名保留，不覆盖旧失败。
最初只做临时archive bytecompare的两轮稳定性报告均实际false：首轮旧archive到新archive的SHA变化，下一轮重复归档仍变化；各归档command本身都成功。原失败/metadata/loghash保留，不将差异擅归因为旧格式或codec变化。最终v5使用实际输入/输出hash缓存，另有一次初始化及两次稳定命中、call完整套件和新Release向量验收，不能把旧false改写为pass。
历史host诊断4组加该增量1组，共5个forced组，事后确认已消失；不能宣称本轮所有进程从未强制清理。no-forced仅适用于各自最终成功矩阵。

**正常控制证据最多128对，准备selftest也占用；到限拒绝新Start，重启不回收，耐久归档/轮换尚未实现。**
A的真实host Child/UDS测试与B的脚本BulkIo/实际ADB parser测试尚未合成B部署→实际A host→DSP/USB→receipt完整端到端验收。
vendor音频库、Linux3.18、PDEATHSIG、目标xattr、真实USB重枚举、DSP和运营商业务仍待实机验证。

本轮只读PCSC存在性观测是ESTKme-RED PRESENT+MUTE、ATR0，context已释放；没有card session、APDU、reset、注册、真实短信/通话或Profile改写。
MUTE不是硬件通过，也不是适用要求N/A。实机/升级回滚/长稳容量/整机性能和完整独立恢复仍待验收。
没有安装候选，没有生成DMG，没有本轮向安装应用发送信号；旧进程交接权限不重复使用。

## 精确清理与证据

所有实际门结束后仅清理三条已核验缓存：Ruby原白名单删除build/rust-target及build/swift-cache；build/module-supervisor-target缺少.rustc_info.json，未伪造marker或放宽产品白名单，而是在核对标准Cargo tag、锁、实际ARM产物SHA、进程及lsof后，通过固定工具链的官方cargo clean对精确target-dir清理。
回收分配空间1807626240字节（1.68GiB），同期可用空间变化1598615552字节，两者不是同一测量。
源码、离线依赖、toolchain、已发布ARM资源、EVS、历史失败/运行数据/基线、候选Products与正式dist均保留并核验。

证据：[最终门禁](../../../../build/reports/iteration-588-gate-summary.json)、[发布核对清单](../../../../build/reports/iteration-588-publication-checklist.md)、[原v2失败](../../../../build/reports/iteration-588-final-v2-daemon-full-result.json)、[rekey夹具修复](../../../../build/reports/iteration-588-rekey-fixture-fix.md)、[只读卡状态](../../../../build/reports/iteration-588-pcsc-presence-observation.json)。
前序：[587](iteration-587-original-voice-observation.md)。[候选验收台账](acceptance-ledger-2026-10-19.md)保持未冻结、零新增产品PASS边界。
