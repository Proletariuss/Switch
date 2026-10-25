# Compile production configuration/enqueue logic with IO replaced. No Keychain,
# service subscription or remote provider is used. Only the checkpoint root
# is injected; production checkpoint IO/authority executes through Rust FFI.
source, destination = ARGV
text = File.read(File.join(source, 'SMSForwardingStore.swift'))
text = text.gsub(/^import (Combine|Security)\n/, '')
subscription = /        BackendProcess\.shared\.\$state\n.*?            \.store\(in: &cancellables\)\n/m
abort 'forwarding subscription marker changed' unless text.scan(subscription).length == 1
text = text.sub(subscription, '')
text = text.gsub(/^    private var cancellables = Set<AnyCancellable>\(\)\n/, '')
# Keep the production read/generate/write adapters. main.swift provides local
# SecItem/SecRandom functions; no Security framework or real Keychain is used.
text = text.gsub('@Published ', '').gsub('private(set) ', '').gsub(/\bprivate /, '')
text = text.gsub('UserDefaults.standard', 'ForwardingFixture.defaults')
scope_factory = 'try SMSForwardingCredentialScope.current()'
abort 'credential scope injection marker changed' unless text.scan(scope_factory).length == 1
text = text.sub(scope_factory, 'try ForwardingFixture.credentialScope()')
text += <<~SWIFT

  @MainActor final class SMSStore {
    static let shared = SMSStore()
    func forwardingAdmissionSnapshot() -> DeviceSMSForwardingHandoff {
      DeviceSMSForwardingHandoff(acceptedNotificationIDs: [], wasEnabled: nil)
    }
  }

  @MainActor final class MultiStandbyStore {
    static let shared = MultiStandbyStore()
    func forwardingAdmissionSnapshot() -> NwaitSMSForwardingHandoff {
      NwaitSMSForwardingHandoff(cursor: nil, seeded: false, wasEnabled: nil)
    }
  }
SWIFT
File.write(File.join(destination, 'SMSForwardingStore.swift'), text)

File.write(File.join(destination, 'SMSForwardingCheckpoint.swift'),
           File.read(File.join(source, 'SMSForwardingCheckpoint.swift')))
