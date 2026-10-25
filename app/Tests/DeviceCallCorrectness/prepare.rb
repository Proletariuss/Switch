source_dir, destination = ARGV
def declaration(source, signature)
  start = source.index(signature) or abort "Missing production declaration: #{signature}"
  finish = source.index('{', start) + 1
  depth = 1
  while depth.positive? && finish < source.length
    depth += 1 if source[finish] == '{'
    depth -= 1 if source[finish] == '}'
    finish += 1
  end
  abort "Unbalanced declaration: #{signature}" unless depth.zero?
  source[start...finish].sub('private func', 'func')
end
network = File.read(File.join(source_dir, 'Views/DiagnosticsView.swift'))
methods = ['private func loadDiagnostic()', 'private func cancelCollection()'].map { |signature| declaration(network, signature) }
at_methods = ['private func sendAT()', 'private func cancelATWait()'].map { |signature| declaration(network, signature) }
card = File.read(File.join(source_dir, 'IncomingCallCard.swift'))
File.write(File.join(destination, 'ExtractedPolicies.swift'), <<~SWIFT)
  import Foundation
  @MainActor final class DiagnosticFixture {
    let backend = BackendProcess()
    var diagnostic: NetworkDiagnostic?
    var diagnosticError: String?
    var isLoading = false
    var collectionTask: Task<Void, Never>?
    var collectionRevision: UInt64 = 0
    var capturedAt: Date?
    #{methods.join("\n")}
  }
  @MainActor final class CardFixture {
    var preview = false
    var number = ""
    var rejectInFlight = false
    var failureMessage: String?
    var answerAvailable = false
    #{declaration(card, 'private func synchronize(store: DashboardStore)')}
  }
  #{declaration(network, 'struct ATEntry: Identifiable')}
  func isModuleDisconnectedError(_ error: String) -> Bool { false }
  let moduleDisconnectedFriendlyMessage = "fixture disconnected"
  @MainActor final class ATConsoleFixture {
    var command = ""
    var history: [ATEntry] = []
    var atBusy = false
    var atError: String?
    var atTask: Task<Void, Never>?
    var atRevision: UInt64 = 0
    #{at_methods.join("\n")}
  }
SWIFT
