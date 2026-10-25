import Foundation
import CryptoKit
import Darwin
import CoreFoundation
import Security

/// Opt-in authority only. This is not a sandbox or a whole-App acceptance receipt.
/// No environment variable can select this lane. The descriptors remain held for
/// the lifetime of the context, and every pathname is rechecked before delegation.
final class AcceptanceContext {
    enum Rejection: Error { case invalid(String) }
    static let argumentPrefix = "--switch-acceptance-capsule="
    static let purpose = "switch-no-device-software-acceptance"
    static let appKeys: Set<String> = [
        "schema_version", "role", "purpose", "run_id", "nonce", "guardian_pid", "uid",
        "created_unix_ms", "expires_unix_ms", "capsule_root", "data_root", "evidence_root", "tmp_root",
        "capsule_root_dev", "capsule_root_ino", "data_root_dev", "data_root_ino",
        "evidence_root_dev", "evidence_root_ino", "tmp_root_dev", "tmp_root_ino",
        "app_path", "app_sha256", "backend_path", "backend_sha256", "guardian_path", "guardian_sha256"
    ]
    let manifest: [String: Any]
    let capsulePath: String
    let manifestSHA256: String
    let manifestInode: UInt64
    let dataRoot: String
    let backendPath: String
    private let heldDirectories: [(String, Int32, stat)]
    private let capsuleFD: Int32
    private let capsuleIdentity: stat

    private init(manifest: [String: Any], path: String, bytes: Data,
                 capsuleFD: Int32, identity: stat, directories: [(String, Int32, stat)]) {
        self.manifest = manifest
        capsulePath = path
        manifestSHA256 = Self.digest(bytes)
        manifestInode = UInt64(identity.st_ino)
        dataRoot = manifest["data_root"] as! String
        backendPath = manifest["backend_path"] as! String
        self.capsuleFD = capsuleFD
        capsuleIdentity = identity
        heldDirectories = directories
    }

    deinit {
        close(capsuleFD)
        for (_, fd, _) in heldDirectories { close(fd) }
    }

    static func capsuleArgument(_ arguments: [String]) throws -> String? {
        let candidates = arguments.dropFirst().filter { $0.hasPrefix("--switch-acceptance") }
        if candidates.isEmpty { return nil }
        guard candidates.count == 1, let argument = candidates.first,
              argument.hasPrefix(argumentPrefix) else { throw Rejection.invalid("acceptance-arguments") }
        let path = String(argument.dropFirst(argumentPrefix.count))
        try canonical(path)
        guard (path as NSString).lastPathComponent == "app.json" else {
            throw Rejection.invalid("parent-manifest-name")
        }
        return path
    }

    /// Pure command-line entry seam: production does not open a file, read a
    /// preference, inspect HOME, or initialize a service here.
    static func load(arguments: [String], executablePath: String, expectedBackendPath: String,
                     environment: [String: String]? = nil,
                     nowMS: Int64 = Int64(Date().timeIntervalSince1970 * 1000),
                     afterRead: (() throws -> Void)? = nil) throws -> AcceptanceContext? {
        guard let path = try capsuleArgument(arguments) else { return nil }
        let parentPath = (path as NSString).deletingLastPathComponent
        let parent = try openDirectory(parentPath)
        var descriptors: [(String, Int32, stat)] = [(parentPath, parent.0, parent.1)]
        var file: Int32 = -1
        var transferred = false
        defer {
            if !transferred {
                if file >= 0 { close(file) }
                for (_, fd, _) in descriptors { close(fd) }
            }
        }
        file = openat(parent.0, (path as NSString).lastPathComponent, O_RDONLY | O_NOFOLLOW | O_CLOEXEC | O_NONBLOCK)
        guard file >= 0 else { throw Rejection.invalid("capsule-open") }
        let before = try fileIdentity(file, privateFile: true)
        guard before.st_size > 0, before.st_size <= 16384 else { throw Rejection.invalid("capsule-size") }
        let bytes = try readAll(file, limit: 16384)
        try afterRead?()
        guard same(before, try fileIdentity(file, privateFile: true)) else {
            throw Rejection.invalid("capsule-changed")
        }
        // Exact canonical JSON also rejects duplicate keys, whitespace variants,
        // escaped-key aliases and trailing data that different decoders interpret
        // differently. Guardian and child delegation use this same encoder.
        guard let manifest = try JSONSerialization.jsonObject(with: bytes) as? [String: Any],
              Set(manifest.keys) == appKeys,
              try encode(manifest) == bytes else { throw Rejection.invalid("capsule-schema") }
        guard try integer(manifest, "schema_version") == 1,
              manifest["role"] as? String == "app", manifest["purpose"] as? String == purpose,
              hex(manifest["run_id"], count: 32), hex(manifest["nonce"], count: 64),
              try integer(manifest, "uid") == Int64(getuid()),
              try integer(manifest, "guardian_pid") == Int64(getppid()), getppid() > 1 else {
            throw Rejection.invalid("capsule-authority")
        }
        let created = try integer(manifest, "created_unix_ms")
        let expires = try integer(manifest, "expires_unix_ms")
        guard created > 0, created <= nowMS, expires > nowMS, expires > created,
              expires - created <= 120000 else { throw Rejection.invalid("capsule-expiry") }
        guard manifest["capsule_root"] as? String == parentPath,
              path == parentPath + "/app.json" else { throw Rejection.invalid("capsule-root") }
        for key in ["capsule_root", "data_root", "evidence_root", "tmp_root"] {
            guard let root = manifest[key] as? String else { throw Rejection.invalid("root-type") }
            try canonical(root)
            if key != "capsule_root" {
                let suffix = ["data_root": "data", "evidence_root": "evidence", "tmp_root": "tmp"][key]!
                guard root == parentPath + "/" + suffix else { throw Rejection.invalid("root-layout") }
                let opened = try openDirectory(root)
                descriptors.append((root, opened.0, opened.1))
            }
            let identity = descriptors.last!.2
            guard try integer(manifest, key + "_dev") == Int64(identity.st_dev),
                  try integer(manifest, key + "_ino") == Int64(identity.st_ino) else {
                throw Rejection.invalid("root-identity")
            }
        }
        guard let app = manifest["app_path"] as? String,
              let backend = manifest["backend_path"] as? String,
              app == executablePath, backend == expectedBackendPath else { throw Rejection.invalid("bundle-image-path") }
        let guardian = try processPath(getppid())
        guard guardian == manifest["guardian_path"] as? String else { throw Rejection.invalid("guardian-image-path") }
        for (binary, hashKey) in [(app, "app_sha256"), (backend, "backend_sha256"), (guardian, "guardian_sha256")] {
            guard hex(manifest[hashKey], count: 64),
                  try hashFile(binary) == manifest[hashKey] as? String else { throw Rejection.invalid("binary-digest") }
        }
        // These variables never authorize the mode. Only after the complete
        // capsule proof do they serve as necessary pre-entry conditions for
        // Foundation/CFPreferences; an OS canary and sandbox are still required.
        let privateEnvironment = environment ?? ProcessInfo.processInfo.environment
        guard privateEnvironment["HOME"] == manifest["data_root"] as? String,
              privateEnvironment["CFFIXED_USER_HOME"] == manifest["data_root"] as? String,
              privateEnvironment["TMPDIR"] == manifest["tmp_root"] as? String else {
            throw Rejection.invalid("private-environment")
        }
        let context = AcceptanceContext(manifest: manifest, path: path, bytes: bytes,
                                        capsuleFD: file, identity: before, directories: descriptors)
        transferred = true
        try context.revalidate()
        // Atomic claim is consumed only after full validation; reuse never falls
        // back to production. This is local replay protection, not UID custody.
        let claim = ".consumed-" + (manifest["nonce"] as! String)
        let claimFD = openat(parent.0, claim, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC, 0o600)
        guard claimFD >= 0 else { throw Rejection.invalid("nonce-reused") }
        defer { close(claimFD) }
        guard fsync(claimFD) == 0, fsync(parent.0) == 0 else { throw Rejection.invalid("claim-sync") }
        return context
    }

    func revalidate() throws {
        for (path, fd, expected) in heldDirectories {
            var actual = stat()
            guard fstat(fd, &actual) == 0, Self.same(expected, actual, directory: true) else {
                throw Rejection.invalid("held-directory-changed")
            }
            let current = try Self.openDirectory(path)
            defer { close(current.0) }
            guard Self.same(expected, current.1, directory: true) else { throw Rejection.invalid("directory-path-race") }
        }
        guard Self.same(capsuleIdentity, try Self.fileIdentity(capsuleFD, privateFile: true)) else {
            throw Rejection.invalid("held-capsule-changed")
        }
        let fd = openat(heldDirectories[0].1, (capsulePath as NSString).lastPathComponent,
                        O_RDONLY | O_NOFOLLOW | O_CLOEXEC | O_NONBLOCK)
        guard fd >= 0 else { throw Rejection.invalid("capsule-path-race") }
        defer { close(fd) }
        guard Self.same(capsuleIdentity, try Self.fileIdentity(fd, privateFile: true)) else {
            throw Rejection.invalid("capsule-path-race")
        }
        guard try Self.integer(manifest, "guardian_pid") == Int64(getppid()),
              try Self.processPath(getppid()) == manifest["guardian_path"] as? String,
              try Self.hashFile(manifest["guardian_path"] as! String) == manifest["guardian_sha256"] as? String,
              try Self.integer(manifest, "expires_unix_ms") > Int64(Date().timeIntervalSince1970 * 1000) else {
            throw Rejection.invalid("authority-expired")
        }
    }

    /// A fresh role-bound child capsule. The ordinary App-created daemon retains
    /// its existing in-memory token and owned Process shutdown path. This does
    /// not claim guardian direct-parent ownership of the daemon grandchild.
    func createDaemonCapsule() throws -> String {
        try revalidate()
        guard try Self.hashFile(backendPath) == manifest["backend_sha256"] as? String else {
            throw Rejection.invalid("backend-drift")
        }
        var child = manifest
        child["role"] = "daemon"
        child["app_pid"] = Int64(getpid())
        child["parent_manifest_sha256"] = manifestSHA256
        child["parent_manifest_inode"] = manifestInode
        child["parent_manifest_path"] = capsulePath
        child["child_nonce"] = try Self.randomHex(bytes: 32)
        let now = Int64(Date().timeIntervalSince1970 * 1000)
        child["created_unix_ms"] = now
        child["expires_unix_ms"] = min(try Self.integer(manifest, "expires_unix_ms"), now + 30000)
        let filename = "daemon-" + (child["child_nonce"] as! String) + ".json"
        let rootFD = heldDirectories.first { $0.0 == dataRoot }!.1
        try Self.writeNew(try Self.encode(child), name: filename, directoryFD: rootFD)
        try revalidate()
        return dataRoot + "/" + filename
    }

    static func canonical(_ path: String) throws {
        guard path.hasPrefix("/"), path != "/", !path.contains("\0"),
              !path.hasSuffix("/"), !path.contains("//"),
              !path.split(separator: "/").contains(where: { $0 == "." || $0 == ".." }) else {
            throw Rejection.invalid("noncanonical-path")
        }
    }

    static func openDirectory(_ path: String) throws -> (Int32, stat) {
        try canonical(path)
        var fd = open("/", O_RDONLY | O_DIRECTORY | O_CLOEXEC)
        guard fd >= 0 else { throw Rejection.invalid("root-open") }
        for component in path.split(separator: "/") {
            let next = openat(fd, String(component), O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
            close(fd)
            guard next >= 0 else { throw Rejection.invalid("directory-open") }
            fd = next
        }
        var identity = stat()
        guard fstat(fd, &identity) == 0, identity.st_mode & S_IFMT == S_IFDIR,
              identity.st_uid == getuid(), identity.st_mode & 0o7777 == 0o700 else {
            close(fd)
            throw Rejection.invalid("directory-owner-mode")
        }
        return (fd, identity)
    }

    static func fileIdentity(_ fd: Int32, privateFile: Bool, expectedOwner: uid_t? = getuid()) throws -> stat {
        var value = stat()
        guard fstat(fd, &value) == 0, value.st_mode & S_IFMT == S_IFREG,
              value.st_nlink == 1, expectedOwner == nil || value.st_uid == expectedOwner,
              !privateFile || [mode_t(0o600), mode_t(0o400)].contains(value.st_mode & 0o7777) else {
            throw Rejection.invalid("file-owner-mode-links")
        }
        return value
    }

    static func same(_ a: stat, _ b: stat, directory: Bool = false) -> Bool {
        a.st_dev == b.st_dev && a.st_ino == b.st_ino && a.st_uid == b.st_uid
            && a.st_mode == b.st_mode && (directory || a.st_nlink == b.st_nlink)
            && (directory || (a.st_size == b.st_size
                && a.st_mtimespec.tv_sec == b.st_mtimespec.tv_sec && a.st_mtimespec.tv_nsec == b.st_mtimespec.tv_nsec
                && a.st_ctimespec.tv_sec == b.st_ctimespec.tv_sec && a.st_ctimespec.tv_nsec == b.st_ctimespec.tv_nsec))
    }

    static func hashFile(_ path: String, afterHash: (() throws -> Void)? = nil) throws -> String {
        try canonical(path)
        // Rewalk from / each time: a held parent FD alone cannot attest the
        // current absolute path after an ancestor is renamed or replaced.
        func openAbsoluteFile() throws -> (Int32, [stat]) {
            let parts = path.split(separator: "/")
            var directory = open("/", O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
            guard directory >= 0 else { throw Rejection.invalid("binary-root") }
            defer { close(directory) }
            var ancestors: [stat] = []
            var identity = stat()
            guard fstat(directory, &identity) == 0 else { throw Rejection.invalid("binary-root-stat") }
            ancestors.append(identity)
            for component in parts.dropLast() {
                let next = openat(directory, String(component), O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
                guard next >= 0 else { throw Rejection.invalid("binary-directory") }
                close(directory)
                directory = next
                guard fstat(directory, &identity) == 0 else { throw Rejection.invalid("binary-directory-stat") }
                ancestors.append(identity)
            }
            let fd = openat(directory, String(parts.last!), O_RDONLY | O_NOFOLLOW | O_CLOEXEC | O_NONBLOCK)
            guard fd >= 0 else { throw Rejection.invalid("binary-open") }
            return (fd, ancestors)
        }
        let (fd, ancestors) = try openAbsoluteFile()
        defer { close(fd) }
        let before = try fileIdentity(fd, privateFile: false, expectedOwner: nil)
        var hash = SHA256()
        var buffer = [UInt8](repeating: 0, count: 65536)
        while true {
            let count = buffer.withUnsafeMutableBytes { read(fd, $0.baseAddress, $0.count) }
            if count < 0 { if errno == EINTR { continue }; throw Rejection.invalid("binary-read") }
            if count == 0 { break }
            hash.update(data: Data(buffer.prefix(count)))
        }
        // Test-only deterministic mutation seam; production callers omit it.
        try afterHash?()
        guard same(before, try fileIdentity(fd, privateFile: false, expectedOwner: nil)) else { throw Rejection.invalid("binary-race") }
        let (current, currentAncestors) = try openAbsoluteFile()
        defer { close(current) }
        guard ancestors.count == currentAncestors.count,
              zip(ancestors, currentAncestors).allSatisfy({ same($0.0, $0.1, directory: true) }),
              same(before, try fileIdentity(current, privateFile: false, expectedOwner: nil)) else {
            throw Rejection.invalid("binary-path-race")
        }
        // This closes the in-function absolute-path recheck only. It provides
        // no immutable custody or binding to a later exec after returning.
        return hash.finalize().map { String(format: "%02x", $0) }.joined()
    }

    static func readAll(_ fd: Int32, limit: Int) throws -> Data {
        var result = Data()
        var buffer = [UInt8](repeating: 0, count: 4096)
        while true {
            let count = buffer.withUnsafeMutableBytes { read(fd, $0.baseAddress, $0.count) }
            if count < 0 { if errno == EINTR { continue }; throw Rejection.invalid("read") }
            if count == 0 { return result }
            result.append(contentsOf: buffer.prefix(count))
            guard result.count <= limit else { throw Rejection.invalid("read-limit") }
        }
    }

    static func writeNew(_ bytes: Data, name: String, directoryFD: Int32) throws {
        let fd = openat(directoryFD, name, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC, 0o600)
        guard fd >= 0 else { throw Rejection.invalid("exclusive-create") }
        defer { close(fd) }
        try bytes.withUnsafeBytes { raw in
            var offset = 0
            while offset < raw.count {
                let count = write(fd, raw.baseAddress!.advanced(by: offset), raw.count - offset)
                if count < 0 { if errno == EINTR { continue }; throw Rejection.invalid("write") }
                guard count > 0 else { throw Rejection.invalid("write-zero") }
                offset += count
            }
        }
        _ = try fileIdentity(fd, privateFile: true)
        guard fsync(fd) == 0, fsync(directoryFD) == 0 else { throw Rejection.invalid("write-sync") }
    }

    static func encode(_ value: [String: Any]) throws -> Data {
        try JSONSerialization.data(withJSONObject: value, options: [.sortedKeys, .withoutEscapingSlashes])
    }
    static func digest(_ data: Data) -> String { SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined() }
    static func randomHex(bytes: Int) throws -> String {
        var data = [UInt8](repeating: 0, count: bytes)
        guard data.withUnsafeMutableBytes({ Security.SecRandomCopyBytes(Security.kSecRandomDefault, $0.count, $0.baseAddress!) }) == 0 else {
            throw Rejection.invalid("entropy")
        }
        return data.map { String(format: "%02x", $0) }.joined()
    }
    static func hex(_ value: Any?, count: Int) -> Bool {
        guard let text = value as? String else { return false }
        return text.utf8.count == count && text.utf8.allSatisfy { (48...57).contains($0) || (97...102).contains($0) }
    }
    static func integer(_ value: [String: Any], _ key: String) throws -> Int64 {
        guard let number = value[key] as? NSNumber,
              CFGetTypeID(number) != CFBooleanGetTypeID(),
              String(cString: number.objCType) != "d", String(cString: number.objCType) != "f",
              number.stringValue == String(number.int64Value), number.int64Value >= 0 else {
            throw Rejection.invalid("integer-" + key)
        }
        return number.int64Value
    }

    static func processPath(_ pid: pid_t) throws -> String {
        var buffer = [CChar](repeating: 0, count: 4 * Int(MAXPATHLEN))
        guard proc_pidpath(pid, &buffer, UInt32(buffer.count)) > 0 else { throw Rejection.invalid("process-image") }
        return String(cString: buffer)
    }
}

/// Lazy in pure fixtures; explicitly forced by SwitchEntry before SwiftUI and
/// NSApplicationDelegateAdaptor can construct any production dependencies.
enum AcceptanceBootstrap {
    static let context: AcceptanceContext? = {
        do {
            // Do not resolve executable identity in the no-argument lane.
            guard try AcceptanceContext.capsuleArgument(CommandLine.arguments) != nil else { return nil }
            let executable = try AcceptanceContext.processPath(getpid())
            guard Bundle.main.executableURL?.path == executable,
                  let resources = Bundle.main.resourceURL else {
                throw AcceptanceContext.Rejection.invalid("bundle-self-image")
            }
            let context = try AcceptanceContext.load(arguments: CommandLine.arguments, executablePath: executable,
                expectedBackendPath: resources.appendingPathComponent("backend/switchd-rs").path)
            // Existing checkpoint stores consume this override directly. Publish
            // only the validated root before their constructors can run.
            guard let context, setenv("SWITCH_DATA_DIR", context.dataRoot, 1) == 0 else {
                throw AcceptanceContext.Rejection.invalid("private-checkpoint-root")
            }
            return context
        } catch {
            // Never print manifest data, paths or secrets in bootstrap errors.
            let message = "Switch acceptance bootstrap refused before service initialization.\n"
            message.withCString { _ = fputs($0, stderr) }
            exit(78)
        }
    }()
    static var isAcceptance: Bool { context != nil }
}
