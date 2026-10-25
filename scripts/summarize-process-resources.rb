#!/usr/bin/env ruby
# Offline, read-only summary of one fixed-PID process-resource journal. This
# reports observations, never an inference about IMS, BLE, battery, or calls.
require 'digest'
require 'json'
require 'optparse'
require 'time'
require_relative 'sample-process-resources'

module ProcessResourceSummary
  class InvalidJournal < StandardError; end

  def self.parse_time(value)
    raise ArgumentError, 'timestamp requires timezone' unless value.is_a?(String) && value.match?(/(?:Z|[+-]\d\d:\d\d)\z/)
    Time.iso8601(value)
  rescue TypeError
    raise ArgumentError, 'invalid timestamp'
  end

  def self.parse_range(value, label: false)
    name, times = label ? value.split('@', 2) : [nil, value]
    raise ArgumentError, 'invalid window label' if label && (!name || !name.match?(/\A[a-zA-Z][a-zA-Z0-9_-]*\z/))
    parts = times.to_s.split('/', 2)
    raise ArgumentError, 'range must be START/END' unless parts.length == 2
    first, last = parts.map { |part| parse_time(part) }
    raise ArgumentError, 'range start must precede end' unless first < last
    { 'label' => name, 'start' => first, 'end' => last }
  end

  def self.read_snapshot(path)
    bytes = File.binread(path)
    last_newline = bytes.rindex("\n")
    raise InvalidJournal, 'journal contains no complete records' unless last_newline
    prefix = bytes.byteslice(0, last_newline + 1)
    rows = prefix.each_line.map { |line| JSON.parse(line) }
    raise InvalidJournal, 'journal records must be objects' unless rows.all? { |row| row.is_a?(Hash) }
    [rows, Digest::SHA256.hexdigest(prefix), prefix.bytesize < bytes.bytesize]
  rescue JSON::ParserError
    raise InvalidJournal, 'malformed complete journal record'
  end

  def self.validate(rows)
    metadata = rows.first
    unless metadata.is_a?(Hash) && metadata['type'] == 'metadata' &&
           metadata['tool'] == ProcessResourceSampler::TOOL &&
           metadata['schema'] == ProcessResourceSampler::SCHEMA
      raise InvalidJournal, 'unsupported journal metadata'
    end
    pids = metadata['pids']
    identities = metadata['identities']
    unless pids.is_a?(Array) && !pids.empty? && pids.all? { |pid| pid.is_a?(Integer) && pid > 0 } &&
           pids.uniq == pids && identities.is_a?(Hash) &&
           pids.all? { |pid| identities[pid.to_s].is_a?(Array) && identities[pid.to_s].length == 2 }
      raise InvalidJournal, 'invalid fixed-PID identity metadata'
    end
    starts = rows.select { |row| row['type'] == 'session_start' }
    raise InvalidJournal, 'expected one original sampling session' unless starts.length == 1
    session = starts.first
    unless session['requested_duration_seconds'].is_a?(Numeric) && session['requested_duration_seconds'] > 0 &&
           session['requested_interval_seconds'].is_a?(Numeric) && session['requested_interval_seconds'] > 0 &&
           session['monotonic_ns'].is_a?(Integer)
      raise InvalidJournal, 'invalid requested duration or interval'
    end
    samples = rows.select { |row| row['type'] == 'sample' }
    ends = rows.select { |row| row['type'] == 'session_end' }
    anomalies = []
    anomalies << 'metadata_or_start_order' unless rows.count { |row| row['type'] == 'metadata' } == 1 && rows[1] == session
    anomalies << 'resume_or_multiple_sessions' unless rows.count { |row| row['type'] == 'resume' }.zero?
    anomalies << 'multiple_terminal_records' if ends.length > 1
    anomalies << 'unexpected_record_type' unless rows.all? { |row| %w[metadata session_start sample session_end resume].include?(row['type']) }
    anomalies << 'missing_samples' if samples.empty?
    anomalies << 'terminal_not_last' if !ends.empty? && rows.last != ends.last
    previous = nil
    samples.each_with_index do |sample, index|
      anomalies << 'sample_sequence_or_session' unless sample['sequence'] == index && sample['session'] == session['session']
      processes = sample['processes']
      unless processes.is_a?(Array) && processes.map { |item| item['pid'] } == pids &&
             processes.all? { |item| item['status'] == 'alive' &&
                                  item.values_at('started_at_utc', 'command') == identities[item['pid'].to_s] }
        anomalies << 'process_identity_or_status'
      end
      anomalies << 'reported_discontinuity' unless sample['discontinuities'] == [] && sample['continuous_so_far'] == true
      begin
        current_at = parse_time(sample.fetch('utc'))
        elapsed_since_start = (sample.fetch('monotonic_ns') - session['monotonic_ns']) / 1_000_000_000.0
        anomalies << 'sample_elapsed_mismatch' unless elapsed_since_start >= 0 &&
                                                    (elapsed_since_start - sample.fetch('elapsed_seconds').to_f).abs <= 0.001
        if previous
          seconds = (sample.fetch('monotonic_ns') - previous.fetch('monotonic_ns')) / 1_000_000_000.0
          wall_seconds = current_at - parse_time(previous.fetch('utc'))
          tolerance = [2.0, session['requested_interval_seconds'] * 0.5].max
          anomalies << 'sample_time_or_interval' unless seconds > 0 && current_at > parse_time(previous.fetch('utc')) &&
                                                       (seconds - sample.fetch('interval_seconds').to_f).abs <= 0.001 &&
                                                       (wall_seconds - seconds).abs <= tolerance &&
                                                       seconds <= session['requested_interval_seconds'] + tolerance
        end
      rescue ArgumentError, KeyError, NoMethodError
        anomalies << 'sample_time_or_interval'
      end
      previous = sample
    end
    last_end = ends.last
    if last_end
      anomalies << 'terminal_session_or_count' unless last_end['session'] == session['session'] && last_end['samples'] == samples.length
      anomalies << 'terminal_continuity' unless last_end['continuous_so_far'] == true
      unless last_end['monotonic_ns'].is_a?(Integer) && last_end['elapsed_seconds'].is_a?(Numeric) &&
             ((last_end['monotonic_ns'] - session['monotonic_ns']) / 1_000_000_000.0 - last_end['elapsed_seconds']).abs <= 0.001
        anomalies << 'terminal_elapsed_mismatch'
      end
      anomalies << 'terminal_before_last_sample' if previous &&
                                                   (!last_end['monotonic_ns'].is_a?(Integer) || last_end['monotonic_ns'] < previous['monotonic_ns'])
    end
    [metadata, session, samples, last_end, anomalies.uniq]
  end

  def self.nearest_rank(values, fraction)
    return nil if values.empty?
    values.sort[[(values.length * fraction).ceil - 1, 0].max].round(4)
  end

  def self.stat(values, weights = nil)
    return nil if values.empty?
    average = if weights
                values.zip(weights).sum { |value, weight| value * weight } / weights.sum
              else
                values.sum.to_f / values.length
              end
    { 'count' => values.length, 'mean' => average.round(4),
      'p50' => nearest_rank(values, 0.50), 'p95' => nearest_rank(values, 0.95),
      'min' => values.min.round(4), 'max' => values.max.round(4) }
  end

  def self.overlap?(left_start, left_end, right_start, right_end)
    left_start < right_end && right_start < left_end
  end

  def self.window_stats(samples, pids, window, exclusions)
    selected = []
    excluded = 0
    invalid = 0
    samples.each_cons(2) do |previous, current|
      begin
        from = parse_time(previous.fetch('utc'))
        to = parse_time(current.fetch('utc'))
      rescue ArgumentError, KeyError
        invalid += 1
        next
      end
      next unless from >= window['start'] && to <= window['end']
      if exclusions.any? { |range| overlap?(from, to, range['start'], range['end']) }
        excluded += 1
        next
      end
      rows = current['processes']
      if current['continuous_so_far'] != true || current['discontinuities'] != [] ||
         !current['total_cpu_percent'].is_a?(Numeric) || !current['total_rss_kib'].is_a?(Numeric) ||
         !rows.is_a?(Array) || rows.map { |row| row['pid'] } != pids ||
         rows.any? { |row| row['status'] != 'alive' || !row['cpu_percent'].is_a?(Numeric) }
        invalid += 1
        next
      end
      selected << [from, to, current]
    end
    cpu = selected.map { |_, _, row| row['total_cpu_percent'] }
    weights = selected.map { |from, to, _| to - from }
    rss = selected.map { |_, _, row| row['total_rss_kib'] }
    per_pid = pids.each_with_object({}) do |pid, result|
      result[pid.to_s] = stat(selected.map { |_, _, sample| sample['processes'].find { |process| process['pid'] == pid }['cpu_percent'] }, weights)
    end
    { 'label' => window['label'], 'requested_start_utc' => window['start'].utc.iso8601(3),
      'requested_end_utc' => window['end'].utc.iso8601(3),
      'valid_interval_count' => selected.length, 'excluded_interval_count' => excluded,
      'invalid_interval_count' => invalid, 'valid_interval_seconds' => weights.sum.round(3),
      'first_valid_interval_start_utc' => selected.first && selected.first[0].utc.iso8601(3),
      'last_valid_interval_end_utc' => selected.last && selected.last[1].utc.iso8601(3),
      'total_cpu_percent' => stat(cpu, weights), 'cpu_percent_by_pid' => per_pid,
      'summed_rss_kib' => stat(rss), 'summed_rss_first_kib' => rss.first,
      'summed_rss_last_kib' => rss.last, 'summed_rss_end_minus_start_kib' => rss.empty? ? nil : rss.last - rss.first }
  end

  def self.summarize(path, exclusions: [], windows: [])
    rows, digest, ignored_tail = read_snapshot(path)
    metadata, session, samples, terminal, anomalies = validate(rows)
    anomalies << 'trailing_partial_record' if ignored_tail
    if terminal && (terminal['duration_complete'] != true || terminal['reason'] != 'duration_reached' ||
                    terminal['elapsed_seconds'].to_f < session['requested_duration_seconds'].to_f)
      anomalies << 'requested_duration_not_completed'
    end
    if terminal && !samples.empty? && samples.last['elapsed_seconds'].to_f < session['requested_duration_seconds'].to_f
      anomalies << 'last_sample_before_requested_duration'
    end
    if windows.empty? && !samples.empty?
      windows = [{ 'label' => 'all_observed_unclassified',
                   'start' => parse_time(samples.first.fetch('utc')),
                   'end' => parse_time(samples.last.fetch('utc')) + 0.000001 }]
    end
    status = if !anomalies.empty?
               'invalid_or_discontinuous'
             elsif terminal.nil?
               'in_progress_or_missing_terminal_record'
             else
               'requested_duration_observed_continuously'
             end
    raw_rss = samples.map { |sample| sample['total_rss_kib'] }.compact
    { 'schema_version' => 1, 'snapshot_sha256' => digest, 'trailing_partial_record_ignored' => ignored_tail,
      'status' => status, 'anomalies' => anomalies.uniq,
      'fixed_pids' => metadata['pids'], 'process_identity_checked' => !samples.empty? && !anomalies.include?('process_identity_or_status'),
      'requested_duration_seconds' => session['requested_duration_seconds'],
      'first_sample_utc' => samples.first && samples.first['utc'],
      'last_sample_utc' => samples.last && samples.last['utc'],
      'last_sample_elapsed_seconds' => samples.last && samples.last['elapsed_seconds'],
      'sample_count' => samples.length,
      'all_observed_summed_rss_first_kib' => raw_rss.first,
      'all_observed_summed_rss_last_kib' => raw_rss.last,
      'all_observed_summed_rss_peak_kib' => raw_rss.max,
      'terminal' => terminal && { 'reason' => terminal['reason'], 'duration_complete' => terminal['duration_complete'],
                                   'continuous_so_far' => terminal['continuous_so_far'],
                                   'elapsed_seconds' => terminal['elapsed_seconds'] },
      'exclusions' => exclusions.map { |range| { 'start_utc' => range['start'].utc.iso8601(3),
                                                  'end_utc' => range['end'].utc.iso8601(3) } },
      'windows' => windows.map { |window| window_stats(samples, metadata['pids'], window, exclusions) },
      'ims_online_verified' => false,
      'limitations' => [
        'A journal without a valid terminal record does not prove its requested duration, even if the process is currently live.',
        'Fixed PID plus ps start time and executable path detects many restarts, not events hidden entirely between samples.',
        'Excluded intervals conservatively remove any CPU sample that overlaps them; remaining intervals are not automatically idle.',
        'Summed RSS may double-count shared pages; endpoint change alone is neither a leak nor a measured benefit.',
        'CPU percentages are relative to one logical core and do not measure energy, wakeups, or UI activity.',
        'IMS OPTIONS/REGISTER, SMS, calls, and BLE require separate business evidence; this summary does not infer them.'
      ] }
  end

  def self.run(argv)
    options = { exclusions: [], windows: [] }
    parser = OptionParser.new do |p|
      p.banner = 'Usage: ruby scripts/summarize-process-resources.rb --journal FILE [--exclude START/END] [--window LABEL@START/END]'
      p.on('--journal FILE') { |value| options[:journal] = value }
      p.on('--exclude RANGE', 'Exclude an explicitly known workload interval') { |value| options[:exclusions] << parse_range(value) }
      p.on('--window RANGE', 'Report an explicitly chosen foreground/background interval') { |value| options[:windows] << parse_range(value, label: true) }
      p.on('--help') { puts p; return 0 }
    end
    parser.parse!(argv)
    raise ArgumentError, 'one --journal and no extra arguments required' unless argv.empty? && options[:journal]
    puts JSON.pretty_generate(summarize(options[:journal], exclusions: options[:exclusions], windows: options[:windows]))
    0
  rescue OptionParser::ParseError, ArgumentError, InvalidJournal, SystemCallError, IOError, TypeError
    warn 'Resource journal could not be summarized; check the input and use --help. No source content is printed.'
    2
  end
end

exit ProcessResourceSummary.run(ARGV) if $PROGRAM_NAME == __FILE__
