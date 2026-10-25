#!/usr/bin/env ruby
# Offline, allowlisted observations only. Never prints source text or identifiers.
require 'date'
require 'json'
require 'optparse'
require 'time'

module VoWiFiLogSummary
  STRUCTURED = {
    register_success: /\AIMS REGISTER 成功(?=\s|\z)/,
    register_failed: /\AIMS register failed(?=\s|\z)/,
    options_sent: /\AIMS OPTIONS ping sent(?=\s|\z)/,
    options_acknowledged: /\AIMS OPTIONS heartbeat acknowledged(?=\s|\z)/,
    options_missed: /\AIMS OPTIONS heartbeat missed; readiness withdrawn while confirming(?=\s|\z)/,
    options_recovered: /\AIMS OPTIONS heartbeat recovered after one missed probe(?=\s|\z)/,
    options_failed: /\AIMS OPTIONS heartbeat failed(?=\s|\z)/,
    runtime_start_failed: /\AIMS transport runtime start failed(?=\s|\z)/,
    port_c_read_ended: /\AIMS port-c read ended(?=\s|\z)/
  }.freeze
  UNSTRUCTURED = {
    lease_scheduled: /\A\[Reader VoWiFi\] IMS registration lease scheduled \(granted=\d+s renew_in=[0-9hms.]+\)\z/,
    lease_renewal_due: /\A\[Reader VoWiFi\] IMS registration lease renewal due \(granted=\d+s\)\z/,
    ims_transport_stopped: /\A\[Reader VoWiFi\] IMS transport stopped:/,
    tunnel_stopped: /\A\[Reader VoWiFi\] Tunnel stopped:/,
    reconnect_attempt_failed: /\A\[Reader VoWiFi\] Automatic reconnect attempt \d+ failed:/,
    dpd_disabled: /\A运营商配置已禁用主动 DPD(?:\s+\{.*\})?\z/
  }.freeze
  EVENTS = (STRUCTURED.keys + UNSTRUCTURED.keys).freeze
  SUCCESS = [:register_success, :options_acknowledged].freeze
  DIAGNOSTICS = [:input_lines, :lines_in_window, :unscoped_timestamp_unmatched_lines,
                 :unscoped_candidate_lines, :timestamp_order_regressions,
                 :event_format_unmatched_lines, :unrelated_lines_in_window,
                 :success_marker_without_final_2xx, :events_without_trace].freeze
  # Broad recognition only diagnoses changed/malformed formats; it never counts success.
  CANDIDATE = /IMS (?:REGISTER|register|OPTIONS|transport|registration)|Tunnel stopped:|Automatic reconnect|主动 DPD/
  STAMP = /\A\[?(\d{4}[-\/]\d{2}[-\/]\d{2}[T ]\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})?)(?:\]\s*|\s+)(.*)\z/
  OFFSET = /\A[+-](?:0\d|1[0-3]):[0-5]\d\z|\A[+-]14:00\z/

  def self.timestamp(value, offset = nil)
    normalized = value.tr('/', '-').sub(' ', 'T')
    normalized += offset.to_s unless normalized.match?(/(?:Z|[+-]\d{2}:\d{2})\z/)
    parts = /\A\d{4}-\d{2}-\d{2}T(\d{2}):(\d{2}):(\d{2})(?:\.\d+)?(Z|[+-]\d{2}:\d{2})\z/.match(normalized)
    raise ArgumentError unless parts && parts[1].to_i < 24 && parts[2].to_i < 60 && parts[3].to_i < 60
    raise ArgumentError unless parts[4] == 'Z' || OFFSET.match?(parts[4])
    Date.iso8601(normalized[0, 10])
    Time.iso8601(normalized)
  end

  def self.bucket
    EVENTS.each_with_object({}) { |event, result| result[event] = [] }
  end

  def self.event_summary(events)
    events.each_with_object({}) do |(event, times), result|
      result[event] = { count: times.length, first_at: times.min&.utc&.iso8601(3), last_at: times.max&.utc&.iso8601(3) }
    end
  end

  def self.evidence(events, start_at, end_at, gap_seconds)
    points = SUCCESS.flat_map { |event| events[event] }.uniq.sort
    gaps = if points.empty?
             [{ from: start_at.utc.iso8601(3), to: end_at.utc.iso8601(3), seconds: (end_at - start_at).round(3), reason: 'no_success_observation' }]
           else
             ([start_at] + points + [end_at]).each_cons(2).map do |left, right|
               next unless right - left > gap_seconds
               { from: left.utc.iso8601(3), to: right.utc.iso8601(3), seconds: (right - left).round(3), reason: 'success_observation_gap_not_proven_outage' }
             end.compact
           end
    ack_times = events[:options_acknowledged].sort
    {
      status: points.empty? ? 'no_success_evidence' : 'observations_only',
      first_success_at: points.first&.utc&.iso8601(3), last_success_at: points.last&.utc&.iso8601(3),
      max_ack_gap_seconds: ack_times.each_cons(2).map { |left, right| right - left }.max&.round(3),
      unknown_gaps: gaps
    }
  end

  def self.summarize(path, start_at, end_at, offset, gap_seconds)
    totals = bucket
    unattributed = bucket
    sessions = {}
    diagnostics = DIAGNOSTICS.each_with_object({}) { |key, result| result[key] = 0 }
    last_timestamp = nil
    last_in_window = nil
    File.foreach(path, encoding: 'UTF-8') do |raw|
      diagnostics[:input_lines] += 1
      line = raw.scrub.gsub(/\e\[[0-9;]*m/, '').strip
      match = STAMP.match(line)
      begin
        at = match && timestamp(match[1], offset)
      rescue ArgumentError
        at = nil
      end
      unless at
        # Scope is unknowable: these counts cover the whole input, not the window.
        diagnostics[:unscoped_timestamp_unmatched_lines] += 1
        diagnostics[:unscoped_candidate_lines] += 1 if line.match?(CANDIDATE)
        next
      end
      next unless at >= start_at && at < end_at
      diagnostics[:lines_in_window] += 1
      diagnostics[:timestamp_order_regressions] += 1 if last_timestamp && at < last_timestamp
      last_timestamp = at
      last_in_window = [last_in_window, at].compact.max
      body = match[2].sub(/\A(?:DEBUG|INFO|WARN|ERROR|DPANIC|PANIC|FATAL)\s+(?:\S+\.go:\d+\s+)?/, '')
      # devicePrefixCore adds [device] when a zap event includes device_id.
      structured_body = body.sub(/\A\[[^\]\r\n]+\]\s+/, '')
      rule = STRUCTURED.find { |_, pattern| pattern.match?(structured_body) }
      fields = {}
      if rule
        event, pattern = rule
        suffix = structured_body[pattern.match(structured_body).end(0)..-1].strip
        begin
          fields = suffix.empty? ? {} : JSON.parse(suffix)
          raise JSON::ParserError unless fields.is_a?(Hash)
        rescue JSON::ParserError
          diagnostics[:event_format_unmatched_lines] += 1
          next
        end
        if SUCCESS.include?(event)
          code = fields[event == :register_success ? 'code' : 'status']
          unless code.is_a?(Integer) && (200...300).cover?(code)
            diagnostics[:success_marker_without_final_2xx] += 1
            next
          end
        end
      else
        rule = UNSTRUCTURED.find { |_, pattern| pattern.match?(body) }
        unless rule
          diagnostics[body.match?(CANDIDATE) ? :event_format_unmatched_lines : :unrelated_lines_in_window] += 1
          next
        end
        event = rule.first
      end
      totals[event] << at
      trace = fields['trace_id']
      if trace.is_a?(String) && !trace.strip.empty?
        session = (sessions[trace] ||= bucket)
        session[event] << at
      else
        diagnostics[:events_without_trace] += 1
        unattributed[event] << at
      end
    end
    associated_success = sessions.values.sum { |events| SUCCESS.sum { |event| events[event].length } }
    {
      schema_version: 1,
      window: { start: start_at.utc.iso8601(3), end: end_at.utc.iso8601(3), end_exclusive: true, naive_log_offset: offset, gap_threshold_seconds: gap_seconds },
      conclusion: associated_success.zero? ? 'insufficient_evidence' : 'observations_only',
      diagnostics: diagnostics,
      last_timestamp_in_window: last_in_window&.utc&.iso8601(3),
      totals: event_summary(totals),
      sessions: sessions.values.each_with_index.map do |events, index|
        { session: index + 1, events: event_summary(events), evidence: evidence(events, start_at, end_at, gap_seconds) }
      end,
      unattributed: { events: event_summary(unattributed), reason: 'trace_missing_or_invalid_no_session_inferred' },
      unknown_window: associated_success.zero? ? [{ from: start_at.utc.iso8601(3), to: end_at.utc.iso8601(3), reason: 'no_attributed_success_observation' }] : [],
      limitations: [
        'Counts are log observations, not deduplicated network transactions; trace association and session numbers exist only within this report.',
        'Missing trace events are not assigned by proximity. Planned renewal and stop markers do not prove successful renewal or the reason for stopping.',
        'No sent-minus-ack loss estimate. Window boundaries and pending probes are not inferred failures.',
        'Unknown gaps use a configurable evidence threshold, not a failure deadline. No absence of errors or silence constitutes a pass.',
        'REGISTER and final OPTIONS 2xx do not prove SMS delivery, incoming calls, media, or continued BLE/APDU availability.',
        'Logs may omit events, rotate, duplicate lines or have clock changes; this offline report does not verify complete collection.'
      ]
    }
  end

  def self.run(argv)
    options = { gap_seconds: 90.0 }
    parser = OptionParser.new do |p|
      p.banner = 'Usage: ruby scripts/summarize-vowifi-log.rb --log FILE --start ISO8601 --end ISO8601 --log-offset +08:00 [--gap-seconds 90]'
      p.on('--log FILE') { |value| options[:path] = value }
      p.on('--start TIME') { |value| options[:start] = value }
      p.on('--end TIME') { |value| options[:end] = value }
      p.on('--log-offset OFFSET', 'Required timezone for timestamps without an offset') { |value| options[:offset] = value }
      p.on('--gap-seconds SECONDS', Float, 'Report longer gaps as unknown, never as inferred loss') { |value| options[:gap_seconds] = value }
      p.on('--help') { puts p; return 0 }
    end
    parser.parse!(argv)
    raise ArgumentError unless argv.empty? && [:path, :start, :end, :offset].all? { |key| options[key] }
    raise ArgumentError unless OFFSET.match?(options[:offset]) && options[:gap_seconds].finite? && options[:gap_seconds] > 0
    start_at = timestamp(options[:start])
    end_at = timestamp(options[:end])
    raise ArgumentError unless start_at < end_at
    puts JSON.pretty_generate(summarize(options[:path], start_at, end_at, options[:offset], options[:gap_seconds]))
    0
  rescue OptionParser::ParseError, ArgumentError
    warn 'Invalid arguments. Use --help; start/end must include a timezone and start must precede end.'
    2
  rescue SystemCallError, IOError
    warn 'Cannot read the input log; no source path or content is included in this error.'
    2
  end
end

exit VoWiFiLogSummary.run(ARGV) if $PROGRAM_NAME == __FILE__
