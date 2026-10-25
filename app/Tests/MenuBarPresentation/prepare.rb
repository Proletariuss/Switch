# Compile only presentation code: never link the application entry point,
# production service stores, sockets, Keychain or launch controllers.
require 'fileutils'
source, destination = ARGV
app = File.read(File.join(source, 'ProletariusApp.swift'))
panel = File.read(File.join(source, 'MenuBarViews.swift'))
label = panel.split('/// 菜单栏标签').first
raise 'Menu bar must have exactly one dynamic Text' unless label.scan(/\bText\(/).length == 1
raise 'Capsule badge returned' if panel.include?('NSColor.systemRed.setFill')
%w[.startPolling( .reconnect(].each do |forbidden|
  raise "Menu bar must not call #{forbidden}" if panel.include?(forbidden)
end
%w[登录时启动 静默启动 菜单栏显示信号 菜单栏显示下载速率 菜单栏显示上传速率 通知设置 下载 退出].each do |label|
  raise "Missing existing action: #{label}" unless panel.include?(label)
end
%w[.home .reader .sms .calls].each do |section|
  raise "Missing navigation: #{section}" unless panel.include?(section)
end
relay = File.read(File.join(source, 'MenuBarStatusSnapshot.swift'))
%w[APIClient URLSession Timer Task Security Keychain].each do |forbidden|
  raise "Snapshot cache must have no I/O: #{forbidden}" if relay.include?(forbidden)
end
# Expose presentation helpers in the disposable test copy; preferences use only
# an isolated suite, never the user's defaults. Production source is untouched.
panel = panel.gsub(/\bprivate /, '').gsub(/@AppStorage\(([^)]+)\)/, '@AppStorage(\1, store: MenuBarFixture.defaults)')
File.write(File.join(destination, 'MenuBarViews.swift'), panel)
types = app.split('struct MenuBarPresentation {', 2).last.split('private struct TrafficCounterSample', 2).first
raise 'Presentation declarations not found' unless types
requests = app[app.index("@MainActor\nfinal class MainWindowRequestCenter")...app.index("@MainActor\nprivate final class AppDependencies")]
raise 'Navigation request center not found' unless requests
methods = app[app.index('    private func readerOperatorLabel')...app.index('    func showMainWindow()')]
raise 'Presentation renderer not found' unless methods
wrapper = <<~SWIFT
  import AppKit
  import SwiftUI
  #{requests}
  @MainActor
  final class AppDelegate: ObservableObject {
      @Published var menuBarPresentation = MenuBarPresentation.loading
      var dashboardStore: DashboardStore?
      var downloadBytesPerSecond: Double? = 4096
      var uploadBytesPerSecond: Double? = 1024
      var signalImageCache: [Int: NSImage] = [:]
      func bindDashboardStore(_ store: DashboardStore) {
          dashboardStore = store
          renderMenuBar(status: store.status, traffic: store.traffic, isStale: store.statusStale)
      }
      func resetTrafficRate() { downloadBytesPerSecond = nil; uploadBytesPerSecond = nil }
      func showMainWindow() { MainWindowRequestCenter.shared.requestOpen() }
  #{methods.gsub(/\bprivate /, '')}
  }
  struct MenuBarPresentation {#{types}
SWIFT
File.write(File.join(destination, 'PresentationRenderer.swift'), wrapper)
puts 'Menu bar source contracts: single dynamic Text; preserved actions; navigation-only business links; no service construction or I/O'
