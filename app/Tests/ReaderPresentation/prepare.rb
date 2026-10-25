# Keep the production ReaderCardView body, actions and modifiers intact. Only
# initialise its disclosure state for this explicit "profiles expanded" fixture.
source_root, output_dir, override_view = ARGV
abort 'usage: prepare.rb SOURCES OUTPUT [READER_VIEW]' unless source_root && output_dir
view_path = override_view && !override_view.empty? ? override_view : File.join(source_root, 'Views/ReaderCardView.swift')
view = File.read(view_path)
required = ['store.connectReader(', 'store.disconnectReader()', 'store.refreshReaderList(',
            'readerESIMStore.switchProfile(', 'readerESIMStore.deleteProfile(',
            'readerShowingDownload = true', 'readerShowingNotifications = true',
            'readerShowingBluetooth = true', 'synchronizeReaderESIM()',
            'privacyMaskEnabled', '.copyableInformation(profile.iccid, enabled: !privacyMaskEnabled)']
required.each { |marker| abort "Reader action/field disappeared: #{marker}" unless view.include?(marker) }
view.sub!(/@State private var readerESIMExpanded = (?:true|false)/,
          '@State private var readerESIMExpanded = true') or abort 'Missing profile disclosure state'
File.write(File.join(output_dir, 'ReaderCardView.swift'), view)

reader_models = File.read(File.join(source_root, 'ReaderModels.swift'))
identities = reader_models[/^struct ReaderInfoResponse:.*?(?=^\/\/\/ POST \/api\/reader\/connect)/m]
vowifi = reader_models[/^struct ReaderVoWiFiStatus:.*?(?=^struct ReaderVoWiFiSessionsResponse)/m]
abort 'Reader model extraction boundary changed' unless identities && vowifi
File.write(File.join(output_dir, 'ReaderValueModels.swift'), "import Foundation\n#{identities}\n#{vowifi}")

# Preserve the actual parent composition, including the former WaterfallLayout.
# Comparing the old card forced into a vertical stack would overstate gains.
view_directory = File.dirname(view_path)
home = File.read(File.join(view_directory, 'HomeView.swift'))
composition = home[/private var readerPageContent: some View \{.*?case \.running:\s*(.*?)\s*case \.starting:/m, 1]
abort 'Reader parent composition extraction boundary changed' unless composition
composition = composition.gsub(/\bvoWiFiCard\b/, 'ReaderCommunicationFixture()')
waterfall = ''
if composition.include?('WaterfallLayout(')
  supports = File.read(File.join(view_directory, 'HomeSupportViews.swift'))
  waterfall = supports[/^struct WaterfallLayout: Layout \{.*\z/m]
  abort 'Baseline WaterfallLayout extraction boundary changed' unless waterfall
end
File.write(File.join(output_dir, 'ReaderFactory.swift'), <<~SWIFT)
  import SwiftUI
  @MainActor func readerFixtureView(store: DashboardStore, masked: Bool) -> some View {
      ReaderFixturePage(store: store, privacyMaskEnabled: masked)
  }
  @MainActor private struct ReaderFixturePage: View {
      @Environment(\\.workspaceViewport) private var viewport
      @ObservedObject var store: DashboardStore
      let privacyMaskEnabled: Bool
      private let reduceMotion = true
      @State private var readerEnabled = true
      var body: some View {
          VStack(alignment: .leading, spacing: viewport.sectionSpacing) {
              #{composition}
          }
      }
  }
  #{waterfall}
SWIFT
puts "Reader production view: #{required.length} action/privacy markers retained; production parent composition; legacy waterfall=#{!waterfall.empty?}"
