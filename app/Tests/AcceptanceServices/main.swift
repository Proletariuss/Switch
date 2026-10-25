import Foundation

@main @MainActor
enum AcceptanceServicesTests {
    static func check(_ value: Bool, file: StaticString = #filePath, line: UInt = #line) {
        precondition(value, "acceptance service boundary failed", file: file, line: line)
    }
    static func main() async {
        AcceptanceBootstrap.isAcceptance = false
        let production = UpdateChecker()
        check(production.autoCheck && production.channel == .stable)
        check(AcceptanceFixtureDefaults.standard.reads > 0 && AcceptanceFixtureDefaults.standard.writes == 1)
        let release = AppRelease(tagName: "v99.1.0", prerelease: false, publishedAt: nil, htmlUrl: nil, assets: nil)
        production.channel = .beta
        production.autoCheck = false
        check(AcceptanceFixtureDefaults.standard.values["updateChannel"] as? String == UpdateChecker.Channel.beta.rawValue)
        check(AcceptanceFixtureDefaults.standard.values["updateAutoCheck"] as? Bool == false)
        production.latestRelease = release
        production.pendingUpdate = release
        production.skipUpdate(release)
        check(AcceptanceFixtureDefaults.standard.values["updateSkippedVersions"] as? [String] == ["99.1.0"])
        check(production.pendingUpdate == nil)
        production.unskipAndPrompt()
        check(AcceptanceFixtureDefaults.standard.values["updateSkippedVersions"] as? [String] == [])
        check(production.pendingUpdate?.tagName == release.tagName)
        production.dismissUpdate()
        check(production.pendingUpdate == nil)
        await production.check()
        check(production.lastError == "更新服务尚未配置")
        check(AutoLaunch.isEnabled)
        try! AutoLaunch.setEnabled(true)
        check(AcceptanceFixtureLoginService.mainApp.observations > 0 && AcceptanceFixtureLoginService.mainApp.mutations == 0)
        let sms = SMSStore(backend: BackendProcess(), attentionStore: AttentionStore())
        check(await sms.ensureAuthorization())
        check(UNUserNotificationCenter.currentCalls > 0)
        let reads = AcceptanceFixtureDefaults.standard.reads
        let writes = AcceptanceFixtureDefaults.standard.writes
        let loginQueries = AcceptanceFixtureLoginService.mainApp.observations
        let notificationQueries = UNUserNotificationCenter.currentCalls
        let fileCalls = AcceptanceFixtureFiles.default.calls
        AcceptanceBootstrap.isAcceptance = true
        let isolated = UpdateChecker()
        check(!isolated.autoCheck && isolated.channel == .stable)
        var mutableEntries: [String] = []
        func zeroPreferenceCalls(_ label: String, _ mutate: () -> Void) {
            mutate()
            check(AcceptanceFixtureDefaults.standard.reads == reads && AcceptanceFixtureDefaults.standard.writes == writes)
            mutableEntries.append(label)
        }
        zeroPreferenceCalls("channel-beta") { isolated.channel = .beta }
        zeroPreferenceCalls("channel-stable") { isolated.channel = .stable }
        zeroPreferenceCalls("autoCheck-true") { isolated.autoCheck = true }
        zeroPreferenceCalls("autoCheck-false") { isolated.autoCheck = false }
        zeroPreferenceCalls("skippedVersions-assignment") { isolated.skippedVersions = ["1.0.0"] }
        zeroPreferenceCalls("skippedVersions-insert") { isolated.skippedVersions.insert("2.0.0") }
        zeroPreferenceCalls("skippedVersions-remove") { isolated.skippedVersions.remove("1.0.0") }
        zeroPreferenceCalls("skipUpdate") { isolated.skipUpdate(release) }
        zeroPreferenceCalls("unskipAndPrompt-without-latest") { isolated.unskipAndPrompt() }
        isolated.latestRelease = release
        isolated.pendingUpdate = release
        zeroPreferenceCalls("skipUpdate-with-pending") { isolated.skipUpdate(release) }
        check(isolated.pendingUpdate?.tagName == release.tagName)
        zeroPreferenceCalls("unskipAndPrompt-with-latest") { isolated.unskipAndPrompt() }
        zeroPreferenceCalls("dismissUpdate") { isolated.dismissUpdate() }
        check(isolated.pendingUpdate == nil)
        // A production-created object must use the same guards when the policy
        // seam changes; production policy itself is immutable after bootstrap.
        zeroPreferenceCalls("existing-object-channel") { production.channel = .stable }
        zeroPreferenceCalls("existing-object-autoCheck") { production.autoCheck = true }
        zeroPreferenceCalls("existing-object-skipUpdate") { production.skipUpdate(release) }
        zeroPreferenceCalls("existing-object-unskipAndPrompt") { production.unskipAndPrompt() }
        await isolated.check()
        await isolated.autoCheckIfNeeded()
        await isolated.check(prompt: true)
        check(isolated.lastError == "隔离验收中更新服务不可用")
        check(AcceptanceFixtureDefaults.standard.reads == reads && AcceptanceFixtureDefaults.standard.writes == writes)
        check(!AutoLaunch.isEnabled)
        do { try AutoLaunch.setEnabled(false); preconditionFailure("login item must refuse") } catch {}
        do { try AutoLaunch.setEnabled(true); preconditionFailure("login item must refuse") } catch {}
        check(AcceptanceFixtureLoginService.mainApp.observations == loginQueries && AcceptanceFixtureLoginService.mainApp.mutations == 0)
        check(AcceptanceFixtureFiles.default.calls == fileCalls && AcceptanceFixtureURLSession.shared.requests == 0)
        check(!(await sms.ensureAuthorization()))
        check(await sms.simulateNotification() == "隔离验收中通知不可用")
        check(UNUserNotificationCenter.currentCalls == notificationQueries)
        print("Mutable entry zero-preference spies: \(mutableEntries.joined(separator: ", "))")
        print("PASS: production UpdateChecker/defaults/login-item/SMS notification behavior; acceptance zero preference/network/login-item/notification boundary calls (in-memory spies)")
    }
}
