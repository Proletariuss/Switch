#!/usr/bin/env ruby
require_relative 'workspace-paths'
require 'json'
require 'open3'
require 'rbconfig'
require 'tmpdir'

SCRIPT = File.join(__dir__, 'summarize-vowifi-log.rb')
SECRET = 'PRIVATE-TRACE-KEY-IDENTITY-13800138000-192.0.2.123'
def require_test(condition, label)
  raise "FAIL #{label}" unless condition
end

def zap(at, message, fields = {})
  "[#{at}] INFO source.go:123 #{message} #{JSON.generate(fields)}\n"
end

def invoke(path, start_at: '2026-09-26T15:00:00+08:00', end_at: '2026-09-26T15:05:00+08:00', extra: [])
  output, error, status = Open3.capture3(RbConfig.ruby, SCRIPT, '--log', path,
                                      '--start', start_at, '--end', end_at,
                                      '--log-offset', '+08:00', *extra)
  require_test(!output.include?(SECRET) && !error.include?(SECRET), 'all output is redacted')
  [status.success? ? JSON.parse(output) : nil, error, status]
end

Dir.mktmpdir('proletarius-log-summary-test-') do |directory|
  path = File.join(directory, SECRET + '.log')
  fields = { 'trace_id' => SECRET, 'status' => 200, 'code' => 200, 'imsi' => SECRET, 'nested' => { SECRET => SECRET } }
  input = zap('2026-09-26 14:59:59', '[reader] IMS REGISTER 成功', fields)
  input += zap('2026-09-26 15:00:00', '[reader] IMS REGISTER 成功', fields)
  input += zap('2026-09-26 15:00:30', 'IMS OPTIONS ping sent', fields)
  input += zap('2026-09-26 15:00:31', 'IMS OPTIONS heartbeat acknowledged', fields)
  input += zap('2026-09-26 15:01:00', 'IMS OPTIONS heartbeat missed; readiness withdrawn while confirming', fields)
  input += zap('2026-09-26 15:01:03', 'IMS OPTIONS heartbeat recovered after one missed probe', fields)
  input += zap('2026-09-26 15:01:03', 'IMS OPTIONS heartbeat acknowledged', fields)
  input += "2026/09/26 15:01:10 [Reader VoWiFi] IMS registration lease scheduled (granted=2400s renew_in=35m0s)\n"
  input += "2026/09/26 15:01:11 [Reader VoWiFi] IMS registration lease renewal due (granted=2400s)\n"
  input += "2026/09/26 15:01:12 [Reader VoWiFi] IMS transport stopped: #{SECRET}\n"
  input += "2026/09/26 15:01:13 [Reader VoWiFi] Tunnel stopped: #{SECRET}\n"
  input += zap('2026-09-26 15:01:14', '运营商配置已禁用主动 DPD')
  input += zap('2026-09-26 15:01:20', 'IMS OPTIONS heartbeat acknowledged', fields.merge('trace_id' => 'another-private-session'))
  input += zap('2026-09-26 15:01:21', 'IMS OPTIONS heartbeat acknowledged', fields.merge('status' => 100))
  input += zap('2026-09-26 15:01:22', 'IMS OPTIONS heartbeat acknowledged', fields.merge('status' => 503))
  input += zap('2026-09-26 15:01:23', 'IMS OPTIONS heartbeat acknowledged', fields.merge('status' => '200'))
  input += zap('2026-09-26 15:01:24', 'IMS OPTIONS heartbeat acknowledged', fields.reject { |key, _| key == 'trace_id' })
  input += zap('2026-09-26 15:01:25', 'IMS OPTIONS heartbeat failed', fields.merge('trace_id' => nil))
  input += zap('2026-09-26 15:01:26', 'IMS OPTIONS heartbeat failed', fields.merge('trace_id' => { 'private' => SECRET }))
  input += "[2026-09-26 15:01:27] WARN source.go:1 IMS OPTIONS heartbeat failed {bad-json-#{SECRET}\n"
  input += zap('2026-09-26 15:01:28', 'ordinary line mentioning IMS OPTIONS heartbeat acknowledged', fields)
  input += "malformed-time IMS OPTIONS heartbeat acknowledged #{SECRET}\n"
  input += zap('2026-09-26 15:04:59', 'IMS OPTIONS ping sent', fields)
  input += zap('2026-09-26 15:05:00', 'IMS OPTIONS heartbeat failed', fields)
  File.write(path, input)
  report, _, status = invoke(path)
  require_test(status.success?, 'mixed fixture exits successfully')
  require_test(report['sessions'].length == 2, 'trace grouping remains internal')
  require_test(!JSON.generate(report).include?('another-private-session'), 'second trace redacted')
  counts = report['totals'].transform_values { |value| value['count'] }
  require_test(counts['register_success'] == 1 && counts['options_acknowledged'] == 4, 'half-open window and final 2xx only')
  require_test(counts['options_missed'] == 1 && counts['options_recovered'] == 1, 'explicit missed and recovery markers')
  require_test(counts['options_failed'] == 2 && counts['lease_scheduled'] == 1 && counts['lease_renewal_due'] == 1, 'explicit failures and lease markers')
  require_test(counts['ims_transport_stopped'] == 1 && counts['tunnel_stopped'] == 1 && counts['dpd_disabled'] == 1, 'stop and DPD markers')
  require_test(report['diagnostics']['events_without_trace'] == 8, 'missing or invalid trace is separate')
  require_test(report['diagnostics']['event_format_unmatched_lines'] == 2, 'bad JSON and embedded marker are not events')
  require_test(report['diagnostics']['unscoped_candidate_lines'] == 1, 'unknown timestamp is not assigned to window')
  require_test(report['diagnostics']['success_marker_without_final_2xx'] == 3, 'unverified success is not counted')
  session = report['sessions'].first
  require_test(session['events']['options_acknowledged']['count'] == 2, 'sessions do not combine ACKs')
  require_test(session['events']['options_failed']['count'] == 0, 'window boundary pending send is not loss')
  require_test(session['evidence']['unknown_gaps'].length == 1, 'quiet tail is unknown')
  require_test(session['evidence']['max_ack_gap_seconds'] == 32.0, 'ACK spacing is observational')
  require_test(session['events']['lease_scheduled']['count'] == 0, 'untraced lease not guessed into session')
  same, _, _ = invoke(path, start_at: '2026-09-26T07:00:00Z', end_at: '2026-09-26T07:05:00Z')
  require_test(same['totals'] == report['totals'], 'equivalent timezone boundaries')

  ['', "no timestamp #{SECRET}\n", zap('2026-09-26 14:00:00', '[reader] IMS REGISTER 成功', fields),
   zap('2026-09-26 15:00:00', 'IMS OPTIONS heartbeat acknowledged', fields.merge('trace_id' => ''))].each do |content|
    File.write(path, content)
    empty, _, result = invoke(path)
    require_test(result.success? && empty['conclusion'] == 'insufficient_evidence', 'empty/unscoped/unattributed is not a pass')
    require_test(empty['unknown_window'].length == 1, 'no evidence leaves the window unknown')
  end

  File.write(path, zap('2026-09-26T07:00:31Z', 'IMS OPTIONS heartbeat acknowledged', fields) +
                   zap('2026-09-26 15:00:30', 'IMS OPTIONS heartbeat acknowledged', fields))
  reordered, _, _ = invoke(path)
  require_test(reordered['totals']['options_acknowledged']['count'] == 2, 'log explicit zone is not offset again')
  require_test(reordered['diagnostics']['timestamp_order_regressions'] == 1, 'clock/order regression is visible')
  require_test(reordered['sessions'].first['evidence']['max_ack_gap_seconds'] == 1.0, 'out-of-order gap is nonnegative')

  File.write(path, "\e[32m[2026-09-26 15:00:00] INFO  pkg/source.go:7   IMS OPTIONS heartbeat acknowledged #{JSON.generate(fields)}\e[0m\n" +
                   "[2026-09-26 15:00:01] INFO source.go:1 IMS OPTIONS heartbeat acknowledged [\"#{SECRET}\"]\n")
  colored, _, _ = invoke(path)
  require_test(colored['totals']['options_acknowledged']['count'] == 1, 'colored padded console envelope')
  require_test(colored['diagnostics']['event_format_unmatched_lines'] == 1 && colored['diagnostics']['events_without_trace'] == 0, 'invalid structured container is not missing trace')

  File.write(path, zap('2026-09-26 15:00:00', '[reader] IMS register failed', fields.merge('device_id' => SECRET)))
  prefixed, _, _ = invoke(path)
  require_test(prefixed['totals']['register_failed']['count'] == 1 && prefixed['diagnostics']['event_format_unmatched_lines'] == 0, 'production devicePrefixCore registration failure')

  [ { start_at: '2026-09-26T15:00:00' }, { end_at: '2026-09-26T15:00:00+08:00' },
    { start_at: '2026-02-30T15:00:00+08:00' }, { start_at: '2026-09-26T15:00:00+07:60' },
    { start_at: '2026-09-25T24:00:00+08:00' }, { extra: ['--log-offset', '+25:00'] },
    { extra: ['--gap-seconds', '0'] }, { extra: ['--unexpected', SECRET] } ].each do |arguments|
    _, _, result = invoke(path, **arguments)
    require_test(result.exitstatus == 2, 'invalid CLI rejected without echo')
  end
  _, _, missing = invoke(File.join(directory, SECRET + '-missing'))
  require_test(missing.exitstatus == 2, 'file errors redact path')
end
puts 'PASS offline log summary: boundaries, final responses, attribution, malformed input, gaps and redaction'
