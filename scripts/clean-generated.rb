#!/usr/bin/env ruby
# Never classify an entire build directory or untracked source as disposable.
require 'fileutils'
require 'find'
require 'json'
require 'open3'
require 'digest'

class GeneratedCacheCleaner
  CACHE_PATHS = %w[Build/Intermediates.noindex CompilationCache.noindex
                   ModuleCache.noindex SDKStatCaches.noindex
                   SDKExplicitPrecompiledModules].freeze
  WORKSPACE_CACHE_PATHS = %w[build/swift-cache build/python-cache
                             build/python-cache-supervisor511 build/cache/pycache].freeze
  HISTORICAL_CAPSULE_LIMIT = 675
  CARGO_CACHE_TAG = 'Signature: 8a477f597d28d172789f06886806bc55'
  DIRECT_CARGO_INCREMENTAL = 'src/rust/target/debug/incremental'

  def initialize(root)
    @root = File.realpath(root)
  end

  def real_directory?(path)
    File.directory?(path) && File.realpath(path) == File.expand_path(path)
  rescue Errno::ENOENT, Errno::ELOOP
    false
  end

  def candidates
    # Only recognised Xcode build roots, never arbitrary source snapshots.
    roots = Dir.glob(File.join(@root, 'build', '*', 'info.plist')) +
            Dir.glob(File.join(@root, 'build', 'optimization', 'DerivedData-*', 'info.plist')) +
            Dir.glob(File.join(@root, 'build', 'dmg-build-*', 'DerivedData', 'info.plist'))
    capsules = Dir.glob(File.join(@root, 'build', 'clean-build', '*')).select do |path|
      recognised_capsule?(path)
    end
    roots.concat(capsules.map { |path| File.join(path, 'build', 'DerivedData', 'info.plist') })
    paths = roots.flat_map do |info|
      directory = File.dirname(info)
      next [] unless real_directory?(directory) && File.file?(info) && !File.symlink?(info)
      next [] if File.exist?(File.join(directory, 'source.tar.gz'))
      next [] unless real_directory?(File.join(directory, 'Build'))
      CACHE_PATHS.map { |suffix| File.join(directory, suffix) }
    end
    cargo_roots = [File.join(@root, 'build', 'rust-target'),
                   File.join(@root, 'src', 'build', 'rust-target'),
                   File.join(@root, 'build', 'handoff-test-target'),
                   File.join(@root, 'build', 'cargo-network-recovery'),
                   File.join(@root, 'build', 'module-supervisor-target')] +
                  capsules.map { |path| File.join(path, 'build', 'rust-target') }
    cargo_roots.each do |cargo|
      marker = File.join(cargo, '.rustc_info.json')
      paths << cargo if File.file?(marker) && !File.symlink?(marker)
      incremental = File.join(cargo, 'debug', 'incremental')
      # workspace-env creates these exact roots before Cargo runs, so Cargo
      # may not create CACHEDIR.TAG. The existing rustc marker plus a strictly
      # compiler-only subtree still lets us retain deps/products during cleanup.
      paths << incremental if cargo_incremental?(cargo, incremental, require_tag: false)
    end
    # The isolated build scripts write Swift/Clang modules here. Only audited
    # capsules qualify; sibling dependency, evidence and product trees do not.
    paths.concat(capsules.map { |path| File.join(path, 'build', 'swift-cache') })
    # Older cold builds also contain a Cargo target and Python bytecode cache.
    # Keep the 675+ release/acceptance capsules intact. In particular, the
    # published ARM artifact and manifest must survive outside the Cargo target.
    capsules.select { |path| historical_capsule?(path) }.each do |capsule|
      target = File.join(capsule, 'build', 'module-supervisor-target')
      pycache = File.join(capsule, 'build', 'python-cache')
      paths << target if historical_supervisor_cache?(capsule, target)
      paths << pycache if historical_python_cache?(pycache)
    end
    incremental = File.join(@root, DIRECT_CARGO_INCREMENTAL)
    paths << incremental if direct_cargo_incremental?(incremental)
    paths.concat(WORKSPACE_CACHE_PATHS.map { |path| File.join(@root, path) })
    paths.select { |path| real_directory?(path) }.uniq.sort.map { |path| relative(path) }
  end

  def recognised_capsule?(path)
    return false unless real_directory?(path)
    return false unless %w[src tools build].all? { |name| real_directory?(File.join(path, name)) }
    audit = File.join(path, 'audit.json')
    return false unless File.file?(audit) && !File.symlink?(audit)
    manifest = JSON.parse(File.read(audit))
    manifest.is_a?(Hash) && manifest['schema'] == 1 && manifest['capsule'] == path
  rescue JSON::ParserError, Errno::ENOENT, Errno::ELOOP
    false
  end

  def historical_capsule?(path)
    match = File.basename(path).match(/\A(?:i|iteration-)(\d+)(?:-|\z)/)
    match && match[1].to_i < HISTORICAL_CAPSULE_LIMIT
  end

  def ordinary_file?(path)
    File.file?(path) && !File.symlink?(path)
  end

  def ordinary_tree?(path, suffix: nil)
    return false unless real_directory?(path)
    files = 0
    Find.find(path) do |entry|
      stat = File.lstat(entry)
      return false if stat.symlink? || (!stat.directory? && !stat.file?)
      if stat.file?
        return false if suffix && File.extname(entry) != suffix
        files += 1
      end
    end
    files.positive?
  rescue Errno::ENOENT, Errno::ELOOP, Errno::EACCES
    false
  end

  def historical_python_cache?(path)
    ordinary_tree?(path, suffix: '.pyc')
  end

  def historical_supervisor_cache?(capsule, target)
    return false unless ordinary_tree?(target)
    return false unless Dir.children(target).sort == %w[CACHEDIR.TAG armv7-unknown-linux-musleabi release].sort
    tag = File.join(target, 'CACHEDIR.TAG')
    return false unless ordinary_file?(tag) && File.open(tag, &:readline).strip == CARGO_CACHE_TAG
    published = File.join(capsule, 'build', 'module-supervisor')
    return false unless real_directory?(published)
    manifest = File.join(published, 'manifest.json')
    artifact = File.join(published, 'switch-voice-supervisor.armv7')
    compiled = File.join(target, 'armv7-unknown-linux-musleabi', 'release', 'switch-voice-supervisor')
    return false unless [manifest, artifact, compiled].all? { |path| ordinary_file?(path) }
    metadata = JSON.parse(File.read(manifest))
    return false unless metadata.is_a?(Hash) && metadata['artifact'] == File.basename(artifact)
    expected = metadata['sha256']
    expected.is_a?(String) && expected.match?(/\A[0-9a-f]{64}\z/) &&
      Digest::SHA256.file(artifact).hexdigest == expected &&
      Digest::SHA256.file(compiled).hexdigest == expected
  rescue JSON::ParserError, EOFError, Errno::ENOENT, Errno::ELOOP, Errno::EACCES
    false
  end

  def direct_cargo_incremental?(path)
    target = File.join(@root, 'src', 'rust', 'target')
    cargo_incremental?(target, path)
  end

  def cargo_incremental?(target, path, require_tag: true)
    debug = File.join(target, 'debug')
    return false unless real_directory?(target) && real_directory?(debug) && real_directory?(path)
    tag = File.join(target, 'CACHEDIR.TAG')
    info = File.join(target, '.rustc_info.json')
    return false unless ordinary_file?(info)
    return false if require_tag && !ordinary_file?(tag)
    return false if File.exist?(tag) && (!ordinary_file?(tag) || File.open(tag, &:readline).strip != CARGO_CACHE_TAG)
    return false unless JSON.parse(File.read(info)).is_a?(Hash)
    # The exact incremental subtree contains only Rust compiler intermediates;
    # sibling debug executables, deps, reports and the whole Cargo target stay.
    files = 0
    Find.find(path) do |entry|
      stat = File.lstat(entry)
      return false if stat.symlink? || (!stat.directory? && !stat.file?)
      if stat.file?
        return false unless %w[.o .bin .lock .rmeta].include?(File.extname(entry))
        files += 1
      end
    end
    files.positive?
  rescue JSON::ParserError, EOFError, Errno::ENOENT, Errno::ELOOP, Errno::EACCES
    false
  end

  def relative(path)
    path.delete_prefix(@root + '/')
  end

  def allocated_bytes(relative_path)
    bytes = 0
    Find.find(File.join(@root, relative_path)) do |path|
      stat = File.lstat(path)
      bytes += stat.blocks * 512 if stat.file?
    end
    bytes
  end

  def active_build_process?(line)
    command, args = line.strip.split(/\s+/, 2)
    name = File.basename(command.to_s)
    return false unless %w[xcodebuild cargo rustc swift swiftc swift-driver swift-frontend
                            clang clang++ clang-cc1 compile link].include?(name)

    # Xcode performs one-time toolchain setup with the same executable name;
    # it does not build a target or hold DerivedData compiler outputs.
    return false if name == 'xcodebuild' && args.to_s.match?(/(?:\A|\s)-runFirstLaunch(?:\s|\z)/)

    true
  end

  def remove!(selected)
    raise '必须指定完整的相对缓存路径；不接受通配符或整个 build/' if selected.empty?
    allowed = candidates
    raise '包含不在缓存白名单内的路径' unless selected.uniq == selected &&
      selected.all? { |path| allowed.include?(path) }
    # A fail-closed guard against deleting a cache used by an active build.
    processes, status = Open3.capture2('/bin/ps', '-axo', 'args=')
    raise '无法确认当前构建进程，取消清理' unless status.success?
    if processes.lines.any? { |line| active_build_process?(line) }
      raise '检测到构建进程，请等待构建结束后再清理'
    end
    selected.each do |path|
      full = File.join(@root, path)
      raise '缓存路径已发生变化，取消清理' unless real_directory?(full) && candidates.include?(path)
      FileUtils.remove_entry_secure(full)
    end
    selected
  end
end

if $PROGRAM_NAME == __FILE__
  cleaner = GeneratedCacheCleaner.new(File.expand_path('../..', __dir__))
  begin
    if ARGV.empty? || ARGV == ['--list']
      entries = cleaner.candidates.map { |path| {path: path, allocated_bytes: cleaner.allocated_bytes(path)} }
      puts JSON.pretty_generate(mode: 'dry_run', caches: entries,
                                note: '保留源码、快照、恢复材料、日志、Build/Products 和 dist。仅 --remove 精确路径 执行清理。')
    elsif ARGV.shift == '--remove'
      puts JSON.pretty_generate(removed: cleaner.remove!(ARGV),
                                recovery: '仅删除可重新编译生成的缓存；首次重建可能较慢。')
    else
      abort 'usage: clean-generated.sh [--list | --remove RELATIVE_CACHE_PATH ...]'
    end
  rescue StandardError => error
    abort error.message
  end
end
