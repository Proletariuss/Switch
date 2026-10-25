import Foundation
import Darwin

/// Shared synthetic fixture builder. Never accepts an App path from the CLI.
/// Real launch remains blocked pending custody, kernel and GUI canary review.
enum GuardianFixture {
    static func selfPath() throws -> String {
        try AcceptanceContext.processPath(getpid())
    }

    static func make(_ root: String, guardianPID: pid_t = getppid()) throws -> (String, [String: Any]) {
        try AcceptanceContext.canonical(root)
        let fm = FileManager.default
        guard !fm.fileExists(atPath: root) else { throw AcceptanceContext.Rejection.invalid("fixture-exists") }
        try fm.createDirectory(atPath: root, withIntermediateDirectories: false, attributes: [.posixPermissions: 0o700])
        for suffix in ["data", "evidence", "tmp"] {
            try fm.createDirectory(atPath: root + "/" + suffix, withIntermediateDirectories: false,
                                   attributes: [.posixPermissions: 0o700])
        }
        let executable = try selfPath()
        let hash = try AcceptanceContext.hashFile(executable)
        let now = Int64(Date().timeIntervalSince1970 * 1000)
        var manifest: [String: Any] = [
            "schema_version": 1, "role": "app", "purpose": AcceptanceContext.purpose,
            "run_id": try AcceptanceContext.randomHex(bytes: 16), "nonce": try AcceptanceContext.randomHex(bytes: 32),
            "guardian_pid": Int64(guardianPID), "uid": Int64(getuid()),
            "created_unix_ms": now, "expires_unix_ms": now + 60000,
            "app_path": executable, "app_sha256": hash, "backend_path": executable, "backend_sha256": hash,
            "guardian_path": try AcceptanceContext.processPath(guardianPID),
            "guardian_sha256": try AcceptanceContext.hashFile(AcceptanceContext.processPath(guardianPID))
        ]
        for (key, suffix) in [("capsule_root", ""), ("data_root", "/data"), ("evidence_root", "/evidence"), ("tmp_root", "/tmp")] {
            let path = root + suffix
            let opened = try AcceptanceContext.openDirectory(path)
            close(opened.0)
            manifest[key] = path
            manifest[key + "_dev"] = Int64(opened.1.st_dev)
            manifest[key + "_ino"] = UInt64(opened.1.st_ino)
        }
        let directory = try AcceptanceContext.openDirectory(root)
        defer { close(directory.0) }
        try AcceptanceContext.writeNew(try AcceptanceContext.encode(manifest), name: "app.json", directoryFD: directory.0)
        return (root + "/app.json", manifest)
    }
}
