#!/usr/bin/env ruby
# Compile the actual small layout policies, without loading the app's backend,
# notification centre or SIM/telephony services into the isolated fixture.
root, output = ARGV

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
  source[start...finish].sub('private var', 'var').sub('private func', 'func')
end

sms = File.read(File.join(root, 'app/Sources/Views/SMSView.swift'))
about = File.read(File.join(root, 'app/Sources/Views/AboutView.swift'))
diagnostics = File.read(File.join(root, 'app/Sources/Views/DiagnosticsView.swift'))
activity = File.read(File.join(root, 'app/Sources/Views/ActivityView.swift'))
abort 'SMS must use the state-preserving layout' unless sms.include?('conversationLayout {')
abort 'About must use the state-preserving layout' unless about.include?('aboutCardsLayout {')
abort 'Activity polling must not replace status identities' if activity.include?('.id(store.unifiedStateRevision)')
quick_bar = declaration(diagnostics, 'private var quickBar:')
abort 'AT errors must not be trapped in the horizontal command scroller' if quick_bar.include?('atError')
abort 'AT Return key must obey the same busy guard as the button' unless diagnostics.include?('guard !cmd.isEmpty, !atBusy else { return }')
abort 'AT errors need a wrapping, selectable presentation' unless diagnostics.match?(/if let atError.*?fixedSize\(horizontal: false, vertical: true\).*?textSelection\(\.enabled\)/m)

policies = %w[usesConversationColumns conversationLayout conversationListWidth].map {
  |name| declaration(sms, "private var #{name}:")
} + %w[usesAboutColumns aboutCardsLayout].map {
  |name| declaration(about, "private var #{name}:")
}
at_send = declaration(diagnostics, 'private func sendAT()')
File.write(output, <<~SWIFT)
  import SwiftUI
  struct PageLayoutPolicy {
      let viewport: WorkspaceViewport
      #{policies.join("\n")}
  }
  @MainActor final class ATConsoleFixture {
      var command = ""
      var atBusy = false
      var atError: String?
      var atTask: Task<Void, Never>?
      var atRevision: UInt64 = 0
      var history: [ATEntry] = []
      #{at_send}
  }
SWIFT
puts 'Page contracts: stable activity identity, exposed AT error, single AT request, unchanged layout state paths passed'
