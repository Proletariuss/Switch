import Foundation
import Darwin

/// Native mechanics only. This executable deliberately has no real-App launch
/// command. A future reviewed launcher can reuse spawnOwned after independently
/// adopting confinement, immutable custody and complete lifetime observation.
@main
enum AcceptanceGuardian {
    static func main() {
        do {
            let args = CommandLine.arguments
            if args.count == 3, args[1] == "--guardian-fixture-child" {
                let context = try AcceptanceContext.load(
                    arguments: [args[0], AcceptanceContext.argumentPrefix + args[2]],
                    executablePath: GuardianFixture.selfPath(), expectedBackendPath: GuardianFixture.selfPath())
                guard let context, getpgrp() == getppid(),
                      ProcessInfo.processInfo.environment["HOME"] == context.dataRoot,
                      ProcessInfo.processInfo.environment["CFFIXED_USER_HOME"] == context.dataRoot,
                      ProcessInfo.processInfo.environment["TMPDIR"] == context.manifest["tmp_root"] as? String else {
                    throw AcceptanceContext.Rejection.invalid("guardian-child")
                }
                let daemon = try context.createDaemonCapsule()
                guard (daemon as NSString).deletingLastPathComponent == context.dataRoot else {
                    throw AcceptanceContext.Rejection.invalid("delegation-path")
                }
                // No App, backend, Keychain, preferences, network or device API.
                exit(0)
            }
            guard args.count == 3, args[1] == "--self-test" else {
                fputs("STOP: only --self-test is implemented; real App launch is NO-GO.\n", stderr)
                exit(78)
            }
            // This process is the actual retained parent and process-group
            // leader. No PGID supplied by JSON is adopted as ownership proof.
            guard (getpgrp() == getpid() || setpgid(0, 0) == 0), getpgrp() == getpid() else {
                throw AcceptanceContext.Rejection.invalid("guardian-process-group")
            }
            let (path, manifest) = try GuardianFixture.make(args[2], guardianPID: getpid())
            let home = manifest["data_root"] as! String
            let pid = try spawnOwned(path: GuardianFixture.selfPath(),
                arguments: ["--guardian-fixture-child", path],
                environment: ["HOME": home, "CFFIXED_USER_HOME": home, "TMPDIR": manifest["tmp_root"] as! String,
                              "SWITCH_DATA_DIR": home,
                              "PATH": "/usr/bin:/bin", "LANG": "C", "LC_ALL": "C"])
            var status: Int32 = 0
            let deadline = ProcessInfo.processInfo.systemUptime + 10
            var reaped = false
            while ProcessInfo.processInfo.systemUptime < deadline {
                let result = waitpid(pid, &status, WNOHANG)
                if result == pid { reaped = true; break }
                guard result == 0 || (result < 0 && errno == EINTR) else {
                    throw AcceptanceContext.Rejection.invalid("waitpid")
                }
                usleep(10000)
            }
            if !reaped {
                // Only our still-unreaped exact child; no broad process search.
                _ = kill(pid, SIGKILL)
                while waitpid(pid, &status, 0) < 0 && errno == EINTR {}
                throw AcceptanceContext.Rejection.invalid("synthetic-child-timeout")
            }
            guard status == 0 else { throw AcceptanceContext.Rejection.invalid("synthetic-child-failed") }
            let report: [String: Any] = [
                "schema_version": 1, "scope": "native-synthetic-self-test-only", "passed": true,
                "guardian_pid": Int64(getpid()), "guardian_pgid": Int64(getpgrp()), "owned_child_pid": Int64(pid),
                "child_waitpid_status": Int(status), "child_reaped": true,
                "private_environment_checked_by_child": true,
                "actual_app_launched": false, "actual_daemon_launched": false,
                "formal_duration_credit_seconds": 0, "g8_g9_runtime_admission": "NO-GO"
            ]
            FileHandle.standardOutput.write(try AcceptanceContext.encode(report))
            FileHandle.standardOutput.write(Data([10]))
        } catch {
            fputs("Guardian synthetic self-test failed: \(error)\n", stderr)
            exit(1)
        }
    }

    private static func spawnOwned(path: String, arguments: [String], environment: [String: String]) throws -> pid_t {
        var attributes: posix_spawnattr_t?
        guard posix_spawnattr_init(&attributes) == 0 else { throw AcceptanceContext.Rejection.invalid("spawn-init") }
        defer { posix_spawnattr_destroy(&attributes) }
        guard posix_spawnattr_setflags(&attributes, Int16(POSIX_SPAWN_SETPGROUP)) == 0,
              posix_spawnattr_setpgroup(&attributes, getpgrp()) == 0 else {
            throw AcceptanceContext.Rejection.invalid("spawn-group")
        }
        let argv = ([path] + arguments).map { strdup($0) } + [nil]
        let envp = environment.sorted { $0.key < $1.key }.map { strdup($0.key + "=" + $0.value) } + [nil]
        defer { for item in argv + envp { if let item { free(item) } } }
        var pid: pid_t = 0
        let result = argv.withUnsafeBufferPointer { argvBuffer in
            envp.withUnsafeBufferPointer { envBuffer in
                posix_spawn(&pid, path, nil, &attributes,
                            UnsafeMutablePointer(mutating: argvBuffer.baseAddress!),
                            UnsafeMutablePointer(mutating: envBuffer.baseAddress!))
            }
        }
        guard result == 0, pid > 1 else { throw AcceptanceContext.Rejection.invalid("spawn") }
        return pid
    }
}
