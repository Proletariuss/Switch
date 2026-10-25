#!/usr/bin/env ruby
# Captures existing dirty/untracked source without modifying git or runtime data.
require 'digest'
require 'fileutils'
require 'json'
require 'open3'
require 'time'

ROOT = File.expand_path('..', __dir__)
mode, destination = ARGV
abort 'usage: ruby scripts/capture-optimization-baseline.rb capture|verify DIRECTORY' unless
  %w[capture verify].include?(mode) && destination && ARGV.length == 2
destination = File.expand_path(destination)
File.umask(0o077)

def command(*args)
  stdout, stderr, status = Open3.capture3(*args, chdir: ROOT)
  raise "command failed: #{args.first}: #{stderr.strip}" unless status.success?
  stdout.strip
end

def source_paths
  paths = command('git', 'ls-files', '-c', '-o', '--exclude-standard', '-z').split("\0").uniq.sort
  # This is a private worktree snapshot, not a guarantee that source itself contains no secrets.
  excluded = %r{\A(?:build/|dist/|\.git/|\.temp/|rust/target/|fixtures/private/|captcha-imeidb\.png\z)|(?:\A|/)(?:\.env(?:\.[^/]*)?|[^/]+\.(?:key|p12|pcap|dmg|log))\z}
  paths.reject { |path| path.match?(excluded) }.select do |path|
    full = File.join(ROOT, path)
    File.file?(full) && !File.symlink?(full)
  end
end

if mode == 'verify'
  manifest = JSON.parse(File.read(File.join(destination, 'manifest.json')))
  expected = manifest.fetch('files')
  changed = expected.each_with_object([]) do |entry, result|
    path = File.join(ROOT, entry.fetch('path'))
    result << entry.fetch('path') unless File.file?(path) && !File.symlink?(path) &&
      Digest::SHA256.file(path).hexdigest == entry.fetch('sha256') &&
      (!entry.key?('mode') || (File.stat(path).mode & 0o777) == entry.fetch('mode'))
  end
  archive = File.join(destination, 'source.tar.gz')
  archive_ok = Digest::SHA256.file(archive).hexdigest == manifest.fetch('archive_sha256')
  added = source_paths - expected.map { |entry| entry.fetch('path') }
  puts JSON.pretty_generate(archive_intact: archive_ok, changed_or_missing_files: changed, added_files: added,
                            file_modes_recorded: expected.all? { |entry| entry.key?('mode') })
  exit(archive_ok && changed.empty? && added.empty? ? 0 : 1)
end

abort 'capture destination must be a NEW directory under build/optimization/' unless
  destination.start_with?(File.join(File.dirname(ROOT), 'build', 'optimization') + '/') && !File.exist?(destination)

files = source_paths
entries = files.map do |path|
  full = File.join(ROOT, path)
  { path: path, size: File.size(full), mode: File.stat(full).mode & 0o777,
    sha256: Digest::SHA256.file(full).hexdigest }
end

FileUtils.mkdir_p(destination, mode: 0o700)
list = File.join(destination, 'source-files.nul')
File.binwrite(list, files.join("\0") + "\0")
archive = File.join(destination, 'source.tar.gz')
command('/usr/bin/tar', '-czf', archive, '-C', ROOT, '--null', '-T', list)
raise 'source changed during snapshot; retain this directory for investigation and retry' unless
  files == source_paths && entries.all? do |entry|
    path = File.join(ROOT, entry[:path])
    Digest::SHA256.file(path).hexdigest == entry[:sha256] && (File.stat(path).mode & 0o777) == entry[:mode]
  end

manifest = {
  captured_at: Time.now.utc.iso8601, root: ROOT,
  version: File.read(File.join(ROOT, 'VERSION')).strip,
  build_number: File.read(File.join(ROOT, 'BUILD_NUMBER')).strip,
  git_head: command('git', 'rev-parse', 'HEAD'),
  expected_variant: 'Switch / macOS 27 / arm64 / Release',
  toolchain: {
    macos: command('/usr/bin/sw_vers'), xcode: command('/usr/bin/xcodebuild', '-version'),
    swift: command('/usr/bin/xcrun', 'swift', '--version'),
    cargo: command('cargo', '--version'), rustc: command('rustc', '--version'),
    architecture: command('/usr/bin/uname', '-m')
  },
  file_count: files.length, source_bytes: entries.sum { |entry| entry[:size] },
  archive_sha256: Digest::SHA256.file(archive).hexdigest,
  files: entries
}
File.write(File.join(destination, 'manifest.json'), JSON.pretty_generate(manifest) + "\n")
File.write(File.join(destination, 'git-status.txt'), command('git', 'status', '--short') + "\n")
puts JSON.pretty_generate(manifest.reject { |key, _| key == :files }.merge(directory: destination))
