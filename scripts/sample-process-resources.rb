#!/usr/bin/env ruby
# Read-only macOS process observation. Does not inspect arguments, environment,
# sockets, application logs, or business data; never starts/stops target processes.
require 'json'
require 'open3'
require 'optparse'
require 'securerandom'
require 'time'

module ProcessResourceSampler
  TOOL = 'proletarius-process-resources'.freeze
  SCHEMA = 1

  class ObservationError < StandardError; end

  def self.cpu_seconds(value)
    match = /\A(?:(\d+)-)?(?:(\d+):)?(\d+):(\d+(?:\.\d+)?)\z/.match(value)
    raise ObservationError, 'unrecognized ps CPU time' unless match
    match[1].to_i * 86_400 + match[2].to_i * 3600 + match[3].to_i * 60 + match[4].to_f
  end

  def self.parse_ps(output, pids)
    output.each_line.each_with_object({}) do |line, processes|
      next if line.strip.empty?
      match = /\A\s*(\d+)\s+(\S+)\s+(\w{3}\s+\w{3}\s+\d{1,2}\s+\d{2}:\d{2}:\d{2}\s+\d{4})\s+(\S+)\s+(\d+)\s+(.+?)\s*\z/.match(line)
      raise ObservationError, 'unrecognized ps row' unless match
      pid = match[1].to_i
      raise ObservationError, 'unexpected or duplicate PID from ps' unless pids.include?(pid) && !processes.key?(pid)
      processes[pid] = {
        'pid' => pid, 'state' => match[2],
        'started_at_utc' => Time.strptime("#{match[3]} UTC", '%a %b %e %H:%M:%S %Y %Z').utc.iso8601,
        'cpu_seconds' => cpu_seconds(match[4]), 'rss_kib' => match[5].to_i,
        'command' => match[6]
      }
    end
  rescue ArgumentError
    raise ObservationError, 'unrecognized ps start time'
  end

  def self.observe(pids)
    output, _error, status = Open3.capture3(
      { 'LC_ALL' => 'C', 'TZ' => 'UTC' }, '/bin/ps', '-p', pids.join(','),
      '-o', 'pid=,stat=,lstart=,time=,rss=,comm=')
    unless status.success? || (status.exitstatus == 1 && output.strip.empty?)
      raise ObservationError, "ps failed (exit #{status.exitstatus})"
    end
    parse_ps(output, pids)
  end

  def self.identity(process)
    process && process.values_at('started_at_utc', 'command')
  end

  class Series
    attr_reader :discontinuous

    def initialize(pids, expected, interval, discontinuous: false)
      @pids, @expected, @interval = pids, expected, interval
      @discontinuous = discontinuous
      @previous = nil
    end

    def sample(observed, monotonic_ns:, utc:, error: nil)
      gaps = []
      elapsed = @previous && (monotonic_ns - @previous['monotonic_ns']) / 1_000_000_000.0
      if elapsed
        wall_elapsed = Time.iso8601(utc) - Time.iso8601(@previous['utc'])
        tolerance = [2.0, @interval * 0.5].max
        gaps << 'sampling_delay' if elapsed > @interval + tolerance
        gaps << 'clock_or_suspend_gap' if elapsed <= 0 || (wall_elapsed - elapsed).abs > tolerance
      end
      gaps << 'observation_failed' if error
      processes = @pids.map do |pid|
        current = observed[pid]
        status = if error
                   'unobserved'
                 elsif current.nil?
                   'missing'
                 elsif current['state'].include?('Z')
                   'exited_zombie'
                 elsif @expected[pid].nil? || ProcessResourceSampler.identity(current) != @expected[pid]
                   'identity_changed'
                 else
                   'alive'
                 end
        gaps << "#{status}:#{pid}" unless status == 'alive'
        row = (current || { 'pid' => pid }).merge('status' => status, 'cpu_percent' => nil)
        previous = @previous && @previous['processes'].find { |item| item['pid'] == pid }
        if status == 'alive' && previous && previous['status'] == 'alive' && elapsed && elapsed > 0
          delta = current['cpu_seconds'] - previous['cpu_seconds']
          if delta < 0
            gaps << "cpu_counter_regressed:#{pid}"
          elsif gaps.empty?
            row['cpu_percent'] = (100.0 * delta / elapsed).round(4)
          end
        end
        row
      end
      # A partial process set or a timing gap must never produce a healthy total.
      processes.each { |row| row['cpu_percent'] = nil } unless gaps.empty?
      @discontinuous ||= !gaps.empty?
      complete_set = processes.all? { |row| row['status'] == 'alive' }
      cpu_available = processes.all? { |row| !row['cpu_percent'].nil? }
      record = {
        'type' => 'sample', 'utc' => utc, 'monotonic_ns' => monotonic_ns,
        'interval_seconds' => elapsed, 'processes' => processes,
        'total_cpu_percent' => cpu_available ? processes.sum { |row| row['cpu_percent'] }.round(4) : nil,
        'total_rss_kib' => complete_set ? processes.sum { |row| row['rss_kib'] } : nil,
        'discontinuities' => gaps, 'continuous_so_far' => !@discontinuous
      }
      record['observation_error'] = error if error
      @previous = record
      record
    end
  end

  class Journal
    attr_reader :metadata, :had_discontinuity

    def initialize(path, resume:)
      flags = File::RDWR | File::NOFOLLOW
      flags |= File::CREAT | File::EXCL unless resume
      @file = File.open(path, flags, 0o600)
      raise ArgumentError, 'output must be a private, regular, single-link file owned by this user' unless
        @file.stat.file? && @file.stat.uid == Process.uid && @file.stat.nlink == 1 && (@file.stat.mode & 0o077).zero?
      raise ArgumentError, 'another sampler owns this output file' unless @file.flock(File::LOCK_EX | File::LOCK_NB)
      @had_discontinuity = false
      if resume
        @file.each_line.with_index do |line, index|
          raise ArgumentError, 'incomplete trailing record; keep original output and choose a new file' unless line.end_with?("\n")
          record = JSON.parse(line)
          @metadata = record if index.zero?
          @had_discontinuity ||= record['continuous_so_far'] == false || record['type'] == 'resume'
        end
        unless @metadata && @metadata['type'] == 'metadata' && @metadata['tool'] == TOOL && @metadata['schema'] == SCHEMA
          raise ArgumentError, 'output is not a supported resource-sampler journal'
        end
      end
      @file.seek(0, IO::SEEK_END)
    rescue StandardError
      @file.close if @file && !@file.closed?
      raise
    end

    def write(record)
      @file.write(JSON.generate(record) + "\n")
      @file.flush
      @file.fsync
    end

    def close
      @file.close
    end
  end

  def self.run(arguments)
    options = { interval: 30.0, duration: nil, resume: false }
    parser = OptionParser.new do |opts|
      opts.banner = 'Usage: ruby scripts/sample-process-resources.rb --pids PID,PID --output FILE --duration SECONDS [--interval 30] [--resume]'
      opts.on('--pids LIST', 'Explicit fixed PID set; never searches by process name') do |value|
        raise OptionParser::InvalidArgument, 'PIDs must be distinct positive integers' unless
          value.match?(/\A[1-9]\d*(?:,[1-9]\d*)*\z/)
        options[:pids] = value.split(',').map(&:to_i).sort
        raise OptionParser::InvalidArgument, 'duplicate PID' unless options[:pids].uniq == options[:pids]
      end
      opts.on('--output FILE', 'New private JSONL file; parent directory must exist') { |value| options[:output] = value }
      opts.on('--interval SECONDS', Float, 'Sampling interval, default 30 seconds') { |value| options[:interval] = value }
      opts.on('--duration SECONDS', Float, 'Required duration for this invocation, including on resume') { |value| options[:duration] = value }
      opts.on('--resume', 'Append a new segment; always records an observation gap') { options[:resume] = true }
      opts.on('-h', '--help') { puts opts; return 0 }
    end
    parser.parse!(arguments)
    raise ArgumentError, 'provide --pids, --output, and --duration; see --help' unless
      arguments.empty? && options[:pids] && options[:output] && options[:duration]
    [:interval, :duration].each do |key|
      raise ArgumentError, "#{key} must be finite and positive" unless options[key].finite? && options[key] > 0
    end

    journal = Journal.new(options[:output], resume: options[:resume])
    if options[:resume] && journal.metadata['pids'] != options[:pids]
      raise ArgumentError, 'resume PID set must match the original journal; use a new file for new processes'
    end
    initial = observe(options[:pids])
    expected = if options[:resume]
                 journal.metadata.fetch('identities').transform_keys(&:to_i)
               else
                 options[:pids].each_with_object({}) { |pid, result| result[pid] = identity(initial[pid]) }
               end
    monotonic = -> { Process.clock_gettime(Process::CLOCK_MONOTONIC, :nanosecond) }
    started = monotonic.call
    session = SecureRandom.uuid
    stamp = { 'utc' => Time.now.utc.iso8601(6), 'monotonic_ns' => started }
    unless options[:resume]
      journal.write(stamp.merge('type' => 'metadata', 'tool' => TOOL, 'schema' => SCHEMA,
                                'pids' => options[:pids], 'identities' => expected,
                                'cpu_unit' => 'seconds', 'rss_unit' => 'KiB',
                                'identity_resolution' => 'ps start time (1 second) plus executable path'))
    end
    journal.write(stamp.merge('type' => options[:resume] ? 'resume' : 'session_start',
                              'session' => session, 'requested_duration_seconds' => options[:duration],
                              'requested_interval_seconds' => options[:interval],
                              'observation_gap' => options[:resume]))
    series = Series.new(options[:pids], expected, options[:interval],
                        discontinuous: options[:resume] || journal.had_discontinuity)
    signal_reader, signal_writer = IO.pipe
    stop_signal = nil
    prior_handlers = {}
    %w[INT TERM].each do |name|
      prior_handlers[name] = Signal.trap(name) do
        stop_signal = name
        begin
          signal_writer.write_nonblock('.')
        rescue IO::WaitWritable, Errno::EPIPE
          # The flag is authoritative; an already readable pipe still wakes select.
        end
      end
    end
    deadline = started + (options[:duration] * 1_000_000_000).to_i
    next_sample = started
    samples = 0
    loop do
      wait_seconds = [0, (next_sample - monotonic.call) / 1_000_000_000.0].max
      IO.select([signal_reader], nil, nil, wait_seconds) if wait_seconds > 0 && !stop_signal
      break if stop_signal
      error = nil
      begin
        observed = samples.zero? ? initial : observe(options[:pids])
      rescue ObservationError => failure
        observed = {}
        error = failure.message
      end
      now = monotonic.call
      record = series.sample(observed, monotonic_ns: now, utc: Time.now.utc.iso8601(6), error: error)
      journal.write(record.merge('session' => session, 'sequence' => samples,
                                 'elapsed_seconds' => (now - started) / 1_000_000_000.0))
      samples += 1
      break if now >= deadline
      next_sample = [now + (options[:interval] * 1_000_000_000).to_i, deadline].min
    end
    finished = monotonic.call
    journal.write('type' => 'session_end', 'session' => session, 'utc' => Time.now.utc.iso8601(6),
                  'monotonic_ns' => finished, 'elapsed_seconds' => (finished - started) / 1_000_000_000.0,
                  'samples' => samples, 'reason' => stop_signal ? "signal_#{stop_signal}" : 'duration_reached',
                  'duration_complete' => stop_signal.nil? && finished >= deadline,
                  'continuous_so_far' => !series.discontinuous, 'ims_online_verified' => false)
    return 130 if stop_signal == 'INT'
    return 143 if stop_signal == 'TERM'
    series.discontinuous ? 2 : 0
  rescue OptionParser::ParseError, ArgumentError, JSON::ParserError, ObservationError, SystemCallError => error
    warn "resource sampler: #{error.message}"
    1
  ensure
    prior_handlers&.each { |name, handler| Signal.trap(name, handler) }
    signal_reader&.close
    signal_writer&.close
    journal&.close
  end
end

exit ProcessResourceSampler.run(ARGV) if $PROGRAM_NAME == __FILE__
