#!/usr/bin/env ruby
require_relative 'workspace-paths'
require 'minitest/autorun'
require 'tmpdir'
require_relative 'sample-process-resources'

class ProcessResourceSamplerTest < Minitest::Test
  def process(pid = 42, cpu: 2.0, started: '2026-09-26T01:00:00Z', command: '/Applications/Test App.app/Test', state: 'S')
    { 'pid' => pid, 'state' => state, 'started_at_utc' => started,
      'command' => command, 'cpu_seconds' => cpu, 'rss_kib' => 2048 }
  end

  def series
    ProcessResourceSampler::Series.new([42], { 42 => ProcessResourceSampler.identity(process) }, 30)
  end

  def sample(target, observed, seconds, utc_seconds = seconds)
    target.sample(observed, monotonic_ns: seconds * 1_000_000_000,
                  utc: (Time.utc(2026, 9, 26) + utc_seconds).iso8601(6))
  end

  def test_parses_macos_cpu_time_and_paths_with_spaces
    assert_in_delta 56.54, ProcessResourceSampler.cpu_seconds('0:56.54'), 0.0001
    assert_in_delta 3662.75, ProcessResourceSampler.cpu_seconds('1:01:02.75'), 0.0001
    assert_in_delta 90_062.75, ProcessResourceSampler.cpu_seconds('1-01:01:02.75'), 0.0001
    row = '42 S Sat Sep 26 01:00:00 2026 0:02.00 2048 /Applications/Test App.app/Test'
    assert_equal process, ProcessResourceSampler.parse_ps(row, [42])[42]
    assert_raises(ProcessResourceSampler::ObservationError) { ProcessResourceSampler.parse_ps(row, [43]) }
  end

  def test_interval_cpu_uses_cumulative_delta_and_monotonic_time
    target = series
    assert_nil sample(target, { 42 => process }, 0)['total_cpu_percent']
    record = sample(target, { 42 => process(cpu: 17.0) }, 30)
    assert_equal 50.0, record['total_cpu_percent']
    assert_equal 2048, record['total_rss_kib']
    assert record['continuous_so_far']
  end

  def test_missing_process_remains_a_discontinuity_after_return
    target = series
    sample(target, { 42 => process }, 0)
    missing = sample(target, {}, 30)
    assert_equal 'missing', missing['processes'][0]['status']
    assert_nil missing['total_cpu_percent']
    assert_nil missing['total_rss_kib']
    resumed = sample(target, { 42 => process(cpu: 3.0) }, 60)
    refute resumed['continuous_so_far']
    assert_nil resumed['total_cpu_percent']
  end

  def test_pid_reuse_executable_change_zombie_and_counter_reset_are_not_success
    [process(started: '2026-09-26T02:00:00Z'), process(command: '/bin/other'), process(state: 'Z')].each do |changed|
      target = series
      sample(target, { 42 => process }, 0)
      record = sample(target, { 42 => changed }, 30)
      refute record['continuous_so_far']
      assert_nil record['total_cpu_percent']
      assert_nil record['total_rss_kib']
    end
    target = series
    sample(target, { 42 => process }, 0)
    record = sample(target, { 42 => process(cpu: 1.0) }, 30)
    assert_includes record['discontinuities'], 'cpu_counter_regressed:42'
    assert_nil record['total_cpu_percent']
  end

  def test_sampling_delay_and_sleep_or_clock_jump_break_continuity
    [[90, 90, 'sampling_delay'], [30, 120, 'clock_or_suspend_gap']].each do |elapsed, wall, code|
      target = series
      sample(target, { 42 => process }, 0)
      record = sample(target, { 42 => process(cpu: 3.0) }, elapsed, wall)
      assert_includes record['discontinuities'], code
      assert_nil record['total_cpu_percent']
      refute record['continuous_so_far']
    end
  end

  def test_ps_failure_is_unknown_not_a_false_exit
    target = series
    record = target.sample({}, monotonic_ns: 0, utc: Time.utc(2026).iso8601, error: 'ps failed')
    assert_equal 'unobserved', record['processes'][0]['status']
    refute record['continuous_so_far']
  end

  def test_real_short_capture_resume_and_private_output
    Dir.mktmpdir('process-resources-test-') do |directory|
      path = File.join(directory, 'resources.jsonl')
      args = ['--pids', Process.pid.to_s, '--output', path, '--duration', '0.05', '--interval', '0.02']
      assert_equal 0, ProcessResourceSampler.run(args.dup)
      original = File.binread(path)
      assert_equal 0o600, File.stat(path).mode & 0o777
      assert_equal 2, ProcessResourceSampler.run(args + ['--resume'])
      assert File.binread(path).start_with?(original)
      records = File.readlines(path).map { |line| JSON.parse(line) }
      assert_equal 1, records.count { |row| row['type'] == 'resume' }
      refute records.last['continuous_so_far']
      assert records.last['duration_complete']
      refute records.last['ims_online_verified']
      assert records.select { |row| row['type'] == 'sample' }.all? { |row| row['processes'].map { |item| item['pid'] } == [Process.pid] }
    end
  end

  def test_symlink_and_concurrent_writer_are_rejected_without_overwrite
    Dir.mktmpdir('process-resources-test-') do |directory|
      path = File.join(directory, 'resources.jsonl')
      first = ProcessResourceSampler::Journal.new(path, resume: false)
      first.write('type' => 'metadata', 'tool' => ProcessResourceSampler::TOOL,
                  'schema' => ProcessResourceSampler::SCHEMA, 'pids' => [42])
      assert_raises(ArgumentError) { ProcessResourceSampler::Journal.new(path, resume: true) }
      first.close
      link = File.join(directory, 'link.jsonl')
      File.symlink(path, link)
      assert_raises(Errno::ELOOP) { ProcessResourceSampler::Journal.new(link, resume: true) }
      assert_equal 1, File.readlines(path).length
    end
  end

  def test_sigterm_flushes_a_resumable_journal_without_touching_target
    Dir.mktmpdir('process-resources-test-') do |directory|
      path = File.join(directory, 'resources.jsonl')
      sampler = Process.spawn(RbConfig.ruby, File.expand_path('sample-process-resources.rb', __dir__),
                              '--pids', Process.pid.to_s, '--output', path, '--duration', '60', '--interval', '30')
      deadline = Process.clock_gettime(Process::CLOCK_MONOTONIC) + 5
      loop do
        break if File.exist?(path) && File.read(path).include?('"type":"sample"')
        raise 'sampler startup timeout' if Process.clock_gettime(Process::CLOCK_MONOTONIC) > deadline
        sleep 0.01
      end
      Process.kill('TERM', sampler)
      _pid, status = Process.wait2(sampler)
      sampler = nil
      assert_equal 143, status.exitstatus
      rows = File.readlines(path).map { |line| JSON.parse(line) }
      assert_equal 'signal_TERM', rows.last['reason']
      refute rows.last['duration_complete']
      resumed = ProcessResourceSampler::Journal.new(path, resume: true)
      assert_equal [Process.pid], resumed.metadata['pids']
      resumed.close
    ensure
      if sampler
        begin
          Process.kill('TERM', sampler)
          Process.wait(sampler)
        rescue Errno::ESRCH, Errno::ECHILD
          # Only the test's sampler subprocess is cleaned up, never the target.
        end
      end
    end
  end
end
