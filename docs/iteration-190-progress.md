# Iteration 190：选中 BLE 读卡器 AKA 身份验证迁入 Rust

日期：2026-10-09。`POST /api/reader/auth` 现在按照选中 reader 类型在Rust中分发PC/SC或BLE鉴权，并在发送AUTH APDU前验证读卡器token、ICCID/IMSI和代次绑定。

BLE实际APDU仍经Swift CoreBluetooth桥传输；Go继续提供generation-bound租约、心跳/释放和显式兼容回退。APDU提交后不自动重放。fake Unix socket桥验证身份匹配时只提交一次AUTH，身份不匹配时在鉴权APDU前终止。

完整迁移门禁见 [Iteration 192](iteration-192-progress.md)。保守计入P2B +0.025计划人周；不代表统一PC/SC owner或完整BLE会话迁移。
