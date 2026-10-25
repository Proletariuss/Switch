#!/usr/bin/env ruby
# Extract only the production View's reader observation/binding methods. The
# fixture executes those methods with the real Store; SwiftUI rendering is not
# simulated, so also assert that both production event hooks call the binding.
source_path, output_path = ARGV
source = File.read(source_path)
hook = /\.onAppear\s*\{\s*synchronizeReaderESIM\(\)\s*\}\s*\.compatibleOnChange\(of: readerESIMContext\)\s*\{ _ in\s*synchronizeReaderESIM\(\)\s*\}/
abort 'ReaderCardView no longer wires appearance + reader context to synchronization' unless source.match?(hook)
abort 'Duplicate legacy eSIM load observer remains in View' if source.include?('.compatibleOnChange(of: store.readerHasESIM)')
abort 'Legacy recovery cancellation must only dismiss its sheet' unless source.match?(
  /Button\("取消"\)\s*\{\s*readerLegacyRecoveryReview = nil\s*\}/)

def declaration(source, signature)
  start = source.index(signature) or abort "Missing production binding: #{signature}"
  brace = source.index('{', start)
  depth = 1
  finish = brace + 1
  while depth.positive? && finish < source.length
    depth += 1 if source[finish] == '{'
    depth -= 1 if source[finish] == '}'
    finish += 1
  end
  abort "Unbalanced declaration: #{signature}" unless depth.zero?
  source[start...finish]
end

context = declaration(source, 'private var readerESIMContext:')
binding = declaration(source, 'private func synchronizeReaderESIM()')
File.write(output_path, <<~SWIFT)
  import Foundation
  @MainActor
  final class ReaderCardViewBindingFixture {
      let store = DashboardStore()
      let readerESIMStore: ReaderESIMStore
      var readerESIMExpanded = false
      var profileListContentHeight: CGFloat = 0
      var readerRenamingProfile: ESIMProfileContext?
      var readerDeletingProfile: ESIMProfileContext?
      var readerEnablingProfile: ESIMProfileContext?
      var readerShowingDownload = false
      var readerShowingNotifications = false

      init(mutationCheckpoints: ESIMMutationCheckpointStore = ESIMMutationCheckpointStore()) {
          readerESIMStore = ReaderESIMStore(mutationCheckpoints: mutationCheckpoints)
      }

      #{context}
      #{binding}

      func deliverContext(name: String?, connected: Bool = true, hasESIM: Bool = true) {
          store.readerName = name
          store.readerConnected = connected
          store.readerHasESIM = hasESIM
          synchronizeReaderESIM()
      }
  }
SWIFT
