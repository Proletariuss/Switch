#!/usr/bin/env ruby
# Compile the actual production stage policy in an isolated Swift fixture.
source_path, dashboard_path, output_path = ARGV
source = File.read(source_path)
dashboard = File.read(dashboard_path)

abort 'Manual reader refresh must force the direct Rust-gateway fallback' unless
  source.include?('store.refreshReaderVoWiFi(force: true)')
abort 'Dashboard fallback must verify reader identity before VoWiFi status' unless
  dashboard.match?(/deviceSnapshot\.refreshReaderFallback\s*=\s*\{[\s\S]*?readerRuntime\.refreshAfterAggregateFailure\(autoConnect: true\)/)
abort 'Dashboard timer must not force duplicate direct polling' unless
  dashboard.match?(/lastVoWiFiHealthRefreshAt[\s\S]*?self\.refreshReaderVoWiFi\(\)/)

def declaration(source, signature)
  start = source.index(signature) or abort "Missing production declaration: #{signature}"
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

abort 'Module progress call must pass actual IMS readiness' unless
  source.match?(/voWiFiProgressSteps\(\s*step: \(st\.progressStep[\s\S]*?imsRegistered: st\.imsRegistered/)
abort 'Module progress call must pass actual SMS readiness' unless
  source.match?(/voWiFiProgressSteps\(\s*step: \(st\.progressStep[\s\S]*?smsReady: st\.smsReady == true/)
abort 'Reader progress call must pass actual IMS readiness' unless
  source.match?(/voWiFiProgressSteps\(\s*step: store\.readerVoWiFiStatus[\s\S]*?imsRegistered: store\.readerVoWiFiStatus\?\.imsState == "registered"/)
abort 'Reader progress call must pass actual SMS readiness' unless
  source.match?(/voWiFiProgressSteps\(\s*step: store\.readerVoWiFiStatus[\s\S]*?smsReady: store\.readerVoWiFiStatus\?\.smsReady == true/)

policy = declaration(source, 'private func progressStageStatus(')
# The fixture calls this extracted method from its test file; only the access
# modifier changes, while the production implementation body stays identical.
policy = policy.sub('private func progressStageStatus(', 'func progressStageStatus(')
File.write(output_path, <<~SWIFT)
  import Foundation
  struct ProgressPolicyFixture {
      enum ProgressStageStatus: Equatable { case pending, current, done, failed }
      #{policy}
  }
SWIFT
