#!/usr/bin/env ruby
require_relative 'workspace-paths'
require 'minitest/autorun'
require 'tmpdir'
require_relative 'summarize-process-resources'

class ProcessResourceSummaryTest < Minitest::Test
  BASE = Time.utc(2026, 9, 26, 9, 10, 57)
  PID = 42
  COMMAND = '/Applications/Private Test.app/Contents/MacOS/Test'
  STARTED = '2026-09-26T09:00:00Z'

  def process(cpu, command: COMMAND)
    { 'pid' => PID, 'state' => 'S', 'started_at_utc' => STARTED,
      'command' => command, 'cpu_seconds' => cpu, 'rss_kib' => 2048 + (cpu * 100).to_i }
  end

  def journal_rows(duration: 60, changed_identity: false, with_end: false)
    identity = ProcessResourceSampler.identity(process(1.0))
    series = ProcessResourceSampler::Series.new([PID], { PID => identity }, 30)
    session = 'fixture-session'
    rows = [
      { 'type' => 'metadata', 'tool' => ProcessResourceSampler::TOOL,
        'schema' => ProcessResourceSampler::SCHEMA, 'pids' => [PID],
        'identities' => { PID.to_s => identity } },
      { 'type' => 'session_start', 'session' => session,
        'requested_duration_seconds' => duration,
        'requested_interval_seconds' => 30.0,
        'monotonic_ns' => 0 }
    ]
    [0, 30, 60].each_with_index do |seconds, index|
      command = changed_identity && index == 1 ? '/Applications/Replacement.app/Test' : COMMAND
      sample = series.sample({ PID => process(1.0 + index * 3.0, command: command) },
                             monotonic_ns: seconds * 1_000_000_000,
                             utc: (BASE + seconds).iso8601(6))
      rows << sample.merge('session' => session, 'sequence' => index,
                           'elapsed_seconds' => seconds.to_f)
    end
    if with_end
      rows << { 'type' => 'session_end', 'session' => session, 'samples' => 3,
                'reason' => 'duration_reached', 'duration_complete' => true,
                'continuous_so_far' => !changed_identity, 'elapsed_seconds' => 60.0,
                'monotonic_ns' => 60_000_000_000 }
    end
    rows
  end

  def with_journal(rows, suffix: '')
    Dir.mktmpdir('soak-summary-test-') do |directory|
      filename = File.join(directory, 'resources.jsonl')
      File.write(filename, rows.map { |row| JSON.generate(row) + "\n" }.join + suffix)
      yield filename
    end
  end

  def test_live_journal_is_not_24_hour_completion_and_hides_raw_identity
    with_journal(journal_rows) do |filename|
      report = ProcessResourceSummary.summarize(filename)
      assert_equal 'in_progress_or_missing_terminal_record', report['status']
      assert_equal 3, report['sample_count']
      assert_equal 60.0, report['last_sample_elapsed_seconds']
      assert_nil report['terminal']
      assert report['process_identity_checked']
      assert_equal 10.0, report['windows'][0]['total_cpu_percent']['mean']
      refute_includes JSON.generate(report), COMMAND
      refute report['ims_online_verified']
    end
  end

  def test_only_valid_terminal_after_duration_proves_requested_process_observation
    with_journal(journal_rows(with_end: true)) do |filename|
      report = ProcessResourceSummary.summarize(filename)
      assert_equal 'requested_duration_observed_continuously', report['status']
      assert_empty report['anomalies']
      assert_equal 60.0, report['terminal']['elapsed_seconds']
    end
    with_journal(journal_rows(duration: 86_400, with_end: true)) do |filename|
      report = ProcessResourceSummary.summarize(filename)
      assert_equal 'invalid_or_discontinuous', report['status']
      assert_includes report['anomalies'], 'requested_duration_not_completed'
    end
  end

  def test_changed_identity_or_missing_sample_sequence_never_passes
    with_journal(journal_rows(changed_identity: true, with_end: true)) do |filename|
      report = ProcessResourceSummary.summarize(filename)
      assert_equal 'invalid_or_discontinuous', report['status']
      assert_includes report['anomalies'], 'process_identity_or_status'
      assert_includes report['anomalies'], 'reported_discontinuity'
    end
    rows = journal_rows(with_end: true)
    rows[3]['sequence'] = 9
    with_journal(rows) do |filename|
      report = ProcessResourceSummary.summarize(filename)
      assert_equal 'invalid_or_discontinuous', report['status']
      assert_includes report['anomalies'], 'sample_sequence_or_session'
    end
  end

  def test_exclusion_removes_any_overlapping_cpu_interval_conservatively
    with_journal(journal_rows) do |filename|
      excluded = ProcessResourceSummary.parse_range("#{(BASE + 20).iso8601}/#{(BASE + 40).iso8601}")
      report = ProcessResourceSummary.summarize(filename, exclusions: [excluded])
      window = report['windows'][0]
      assert_equal 0, window['valid_interval_count']
      assert_equal 2, window['excluded_interval_count']
      assert_nil window['total_cpu_percent']
      assert_nil window['summed_rss_end_minus_start_kib']
    end
  end

  def test_explicit_window_is_labeled_but_not_automatically_idle
    with_journal(journal_rows) do |filename|
      range = ProcessResourceSummary.parse_range("background@#{BASE.iso8601}/#{(BASE + 61).iso8601}", label: true)
      report = ProcessResourceSummary.summarize(filename, windows: [range])
      assert_equal 'background', report['windows'][0]['label']
      assert_equal 60.0, report['windows'][0]['valid_interval_seconds']
      assert report['limitations'].any? { |line| line.include?('not automatically idle') }
    end
  end

  def test_trailing_partial_line_and_malformed_record_cannot_claim_completion
    with_journal(journal_rows(with_end: true), suffix: '{"type":"sample"') do |filename|
      report = ProcessResourceSummary.summarize(filename)
      assert_equal 'invalid_or_discontinuous', report['status']
      assert report['trailing_partial_record_ignored']
      assert_includes report['anomalies'], 'trailing_partial_record'
    end
    with_journal(journal_rows, suffix: "not-json\n") do |filename|
      assert_raises(ProcessResourceSummary::InvalidJournal) { ProcessResourceSummary.summarize(filename) }
    end
  end
end
