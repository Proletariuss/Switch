#!/usr/bin/env ruby
require_relative 'workspace-paths'
require 'tmpdir'
require 'fileutils'
require_relative 'clean-generated'

def write_fixture(root, path, content = 'keep')
  full = File.join(root, path)
  FileUtils.mkdir_p(File.dirname(full))
  File.write(full, content)
end

def reject_removal(cleaner, paths)
  begin
    cleaner.remove!(paths)
  rescue RuntimeError
    return
  end
  raise "accepted unsafe removal: #{paths.inspect}"
end

def remove_in_fixture(cleaner, paths)
  original_capture = Open3.method(:capture2)
  begin
    # Fixture deletion tests must be independent of other workspace builds.
    Open3.define_singleton_method(:capture2) { |*_| ['', Struct.new(:success?).new(true)] }
    cleaner.remove!(paths)
  ensure
    Open3.define_singleton_method(:capture2, original_capture)
  end
end

Dir.mktmpdir('proletarius-clean-test') do |root|
  derived = File.join(root, 'build/optimization/DerivedData-fixture')
  protected = File.join(root, 'build/optimization/source-snapshot')
  outside = File.join(root, 'outside')
  [derived + '/Build/Products', derived + '/Build/Intermediates.noindex', protected, outside].each do |path|
    FileUtils.mkdir_p(path)
  end
  File.write(derived + '/info.plist', 'fixture')
  File.write(derived + '/Build/Products/app', 'application')
  File.write(derived + '/Build/Intermediates.noindex/cache', 'generated')
  File.write(protected + '/source.tar.gz', 'irreplaceable source')
  File.write(outside + '/keep', 'external')
  File.symlink(outside, derived + '/ModuleCache.noindex')
  File.symlink(outside, derived + '/Build/Intermediates.noindex/foreign')
  cleaner = GeneratedCacheCleaner.new(root)
  expected = 'build/optimization/DerivedData-fixture/Build/Intermediates.noindex'
  raise 'Xcode first-launch setup was misclassified as a build' if
    cleaner.active_build_process?('/Applications/Xcode.app/Contents/Developer/usr/bin/xcodebuild /Applications/Xcode.app/Contents/Developer/usr/bin/xcodebuild -runFirstLaunch')
  raise 'actual Xcode build was not guarded' unless
    cleaner.active_build_process?('/Applications/Xcode.app/Contents/Developer/usr/bin/xcodebuild /Applications/Xcode.app/Contents/Developer/usr/bin/xcodebuild -project Switch.xcodeproj build')
  raise 'unsafe candidate list' unless cleaner.candidates == [expected]
  raise 'dry-run removed files' unless File.read(protected + '/source.tar.gz') == 'irreplaceable source'
  ['build', 'build/optimization', 'rust/target', '../outside', expected + '/../Products'].each do |invalid|
    rejected = false
    begin
      cleaner.remove!([invalid])
    rescue RuntimeError
      rejected = true
    end
    raise "accepted unsafe path: #{invalid}" unless rejected
  end
  remove_in_fixture(cleaner, [expected])
  raise 'cache not removed' if File.exist?(File.join(root, expected))
  raise 'backup changed' unless File.read(protected + '/source.tar.gz') == 'irreplaceable source'
  raise 'product changed' unless File.read(derived + '/Build/Products/app') == 'application'
  raise 'followed nested symlink' unless File.read(outside + '/keep') == 'external'
  # An entire apparently valid DerivedData root reached through a symlink is excluded.
  File.symlink(derived, File.join(root, 'build/LinkedBuild'))
  raise 'followed root symlink' unless cleaner.candidates.empty?
end

Dir.mktmpdir('proletarius-clean-capsule-test') do |root|
  capsule = 'build/clean-build/fixture'
  expected = GeneratedCacheCleaner::WORKSPACE_CACHE_PATHS.dup
  expected.each { |path| write_fixture(root, path + '/generated') }
  FileUtils.mkdir_p(File.join(root, capsule, 'src'))
  FileUtils.mkdir_p(File.join(root, capsule, 'tools'))
  write_fixture(root, capsule + '/audit.json', JSON.generate(schema: 1, capsule: File.join(root, capsule)))
  swift_cache = capsule + '/build/swift-cache'
  write_fixture(root, swift_cache + '/Foundation.swiftmodule')
  expected << swift_cache
  write_fixture(root, capsule + '/build/DerivedData/info.plist')
  GeneratedCacheCleaner::CACHE_PATHS.each do |suffix|
    path = capsule + '/build/DerivedData/' + suffix
    write_fixture(root, path + '/generated')
    expected << path
  end
  cargo = capsule + '/build/rust-target'
  write_fixture(root, cargo + '/.rustc_info.json', '{}')
  write_fixture(root, cargo + '/release/generated')
  expected << cargo
  source_cargo = 'src/build/rust-target'
  write_fixture(root, source_cargo + '/.rustc_info.json', '{}')
  write_fixture(root, source_cargo + '/debug/generated')
  expected << source_cargo
  %w[build/handoff-test-target build/cargo-network-recovery].each do |target|
    write_fixture(root, target + '/.rustc_info.json', '{}')
    write_fixture(root, target + '/debug/generated')
    expected << target
  end
  protected = %W[#{capsule}/src/source.rs #{capsule}/tools/compiler
                 #{capsule}/dist/Switch.app/application #{capsule}/build.log
                 #{capsule}/execution.json #{capsule}/artifact-audit.json
                 #{capsule}/build/cargo-home/registry/dependency
                 #{capsule}/build/runtime/state #{capsule}/build/evs/reference
                 #{capsule}/build/reports/evidence #{capsule}/build/baselines/rollback
                 #{capsule}/build/DerivedData/Build/Products/application
                 #{capsule}/build/DerivedData/Logs/evidence
                 build/cargo-home/registry/dependency build/runtime/state build/tmp/diagnostic.py
                 build/evs/reference build/python-cache-unrecognised/keep build/cache/other/keep
                 src/build/optimization/measurement.json src/build/runtime/state]
  protected.each { |path| write_fixture(root, path) }

  # Unmarked or misidentified capsules cannot expand the deletion whitelist.
  invalid = 'build/clean-build/unrecognised'
  write_fixture(root, invalid + '/build/swift-cache/keep')
  write_fixture(root, invalid + '/build/rust-target/.rustc_info.json', '{}')
  write_fixture(root, invalid + '/build/DerivedData/info.plist')
  write_fixture(root, invalid + '/build/DerivedData/Build/Intermediates.noindex/keep')
  %w[src tools].each { |name| FileUtils.mkdir_p(File.join(root, invalid, name)) }
  invalid_audit = invalid + '/audit.json'
  write_fixture(root, invalid_audit, 'malformed JSON')
  cleaner = GeneratedCacheCleaner.new(root)
  raise 'unsafe extended candidate list' unless cleaner.candidates == expected.sort
  write_fixture(root, invalid_audit, JSON.generate(schema: 1, capsule: '/different/location'))
  raise 'accepted wrong capsule identity' unless cleaner.candidates == expected.sort
  reject_removal(cleaner, [invalid + '/build/swift-cache'])

  # A valid capsule reached through a symlink is never selected.
  File.symlink(File.join(root, capsule), File.join(root, 'build/clean-build/linked'))
  raise 'followed capsule symlink' unless cleaner.candidates == expected.sort
  linked_build = 'build/clean-build/linked-build'
  %w[src tools].each { |name| FileUtils.mkdir_p(File.join(root, linked_build, name)) }
  write_fixture(root, linked_build + '/audit.json',
                JSON.generate(schema: 1, capsule: File.join(root, linked_build)))
  File.symlink(File.join(root, capsule, 'build'), File.join(root, linked_build, 'build'))
  raise 'followed capsule build symlink' unless cleaner.candidates == expected.sort
  reject_removal(cleaner, [linked_build + '/build/swift-cache'])
  # Swift and Clang subprocesses can write the module cache without cargo or
  # xcodebuild remaining in the process list.
  %w[swift swiftc swift-driver swift-frontend clang clang++ clang-cc1].each do |name|
    raise "active compiler was not guarded: #{name}" unless
      cleaner.active_build_process?("/usr/bin/#{name} -module-cache-path #{swift_cache}")
  end
  original_capture = Open3.method(:capture2)
  begin
    Open3.define_singleton_method(:capture2) { |*_| ["swift-frontend -module-cache-path #{swift_cache}\n", Struct.new(:success?).new(true)] }
    reject_removal(cleaner, expected)
    raise 'active build guard deleted files' unless expected.all? { |path| File.directory?(File.join(root, path)) }
    Open3.define_singleton_method(:capture2) { |*_| ['', Struct.new(:success?).new(false)] }
    reject_removal(cleaner, expected)
  ensure
    Open3.define_singleton_method(:capture2, original_capture)
  end

  # Reject the entire request before deleting anything if any entry is unsafe.
  reject_removal(cleaner, [expected.first, 'build/runtime'])
  reject_removal(cleaner, [expected.first, expected.first])
  reject_removal(cleaner, [])
  reject_removal(cleaner, [capsule + '/build/DerivedData'])
  reject_removal(cleaner, [capsule + '/build'])
  reject_removal(cleaner, [capsule + '/build/swift-cache/../../src'])
  reject_removal(cleaner, [invalid + '/build/rust-target'])
  raise 'partial unsafe request deleted cache' unless File.directory?(File.join(root, expected.first))

  # No aliases through a symlinked parent or cache root, even at exact allowed paths.
  cache_path = File.join(root, 'build/python-cache-supervisor511')
  FileUtils.remove_entry_secure(cache_path)
  File.symlink(File.join(root, capsule, 'src'), cache_path)
  expected.delete('build/python-cache-supervisor511')
  reject_removal(cleaner, ['build/python-cache-supervisor511'])
  FileUtils.remove_entry_secure(File.join(root, 'build/cache'))
  File.symlink(File.join(root, capsule, 'src'), File.join(root, 'build/cache'))
  expected.delete('build/cache/pycache')
  reject_removal(cleaner, ['build/cache/pycache'])
  protected.delete('build/cache/other/keep')

  # A symlink at the exact Swift cache path cannot alias capsule source data.
  FileUtils.remove_entry_secure(File.join(root, swift_cache))
  File.symlink(File.join(root, capsule, 'src'), File.join(root, swift_cache))
  expected.delete(swift_cache)
  reject_removal(cleaner, [swift_cache])

  # Nested symlinks are unlinked without following their external targets.
  outside = File.join(root, 'external-data')
  FileUtils.mkdir_p(outside)
  File.write(File.join(outside, 'keep'), 'keep')
  File.symlink(outside, File.join(root, expected.first, 'foreign'))
  raise 'dry-run changed protected data' unless protected.all? { |path| File.read(File.join(root, path)) == 'keep' }
  remove_in_fixture(cleaner, expected)
  raise 'extended cache not removed' if expected.any? { |path| File.exist?(File.join(root, path)) }
  raise 'protected capsule data changed' unless protected.all? { |path| File.read(File.join(root, path)) == 'keep' }
  raise 'followed nested symlink' unless File.read(File.join(outside, 'keep')) == 'keep'
  raise 'unexpected residual candidates' unless cleaner.candidates.empty?
end
Dir.mktmpdir('proletarius-clean-historical-capsule-test') do |root|
  old = 'build/clean-build/iteration-674-cold-build'
  recent = 'build/clean-build/iteration-675-cold-build'
  [old, recent].each do |capsule|
    %w[src tools build].each { |name| FileUtils.mkdir_p(File.join(root, capsule, name)) }
    write_fixture(root, capsule + '/audit.json',
                  JSON.generate(schema: 1, capsule: File.join(root, capsule)))
  end
  artifact_data = 'compiled ARM artifact'
  digest = Digest::SHA256.hexdigest(artifact_data)
  setup_target = lambda do |capsule|
    base = capsule + '/build/module-supervisor-target'
    write_fixture(root, base + '/CACHEDIR.TAG', GeneratedCacheCleaner::CARGO_CACHE_TAG + "\n")
    write_fixture(root, base + '/release/.cargo-lock', '')
    write_fixture(root, base + '/armv7-unknown-linux-musleabi/release/switch-voice-supervisor',
                  artifact_data)
    write_fixture(root, capsule + '/build/module-supervisor/switch-voice-supervisor.armv7',
                  artifact_data)
    write_fixture(root, capsule + '/build/module-supervisor/manifest.json',
                  JSON.generate(artifact: 'switch-voice-supervisor.armv7', sha256: digest))
    write_fixture(root, capsule + '/build/python-cache/Users/test/module.pyc', 'bytecode')
  end
  setup_target.call(old)
  setup_target.call(recent)
  protected = [old + '/src/source.rs', old + '/tools/compiler', old + '/build/build.log',
               old + '/build/module-supervisor/switch-voice-supervisor.armv7',
               old + '/build/module-supervisor/manifest.json', recent + '/build/build.log']
  protected.each { |path| write_fixture(root, path, 'keep') unless File.exist?(File.join(root, path)) }
  cleaner = GeneratedCacheCleaner.new(root)
  old_target = old + '/build/module-supervisor-target'
  old_pycache = old + '/build/python-cache'
  expected = [old_target, old_pycache].sort
  raise 'historical cache list included newer release capsule' unless cleaner.candidates == expected

  # A missing or mismatched separately published ARM artifact must retain the
  # Cargo target; Python cache identification is independent of that artifact.
  published = File.join(root, old, 'build/module-supervisor/switch-voice-supervisor.armv7')
  File.write(published, 'changed')
  raise 'accepted unbacked supervisor target' unless cleaner.candidates == [old_pycache]
  File.write(published, artifact_data)
  write_fixture(root, old_pycache + '/unexpected.log', 'retain')
  raise 'accepted non-bytecode Python cache content' unless cleaner.candidates == [old_target]
  File.unlink(File.join(root, old_pycache, 'unexpected.log'))
  File.symlink(File.join(root, old, 'src'), File.join(root, old_target, 'foreign'))
  raise 'accepted symlink in compiler target' unless cleaner.candidates == [old_pycache]
  File.unlink(File.join(root, old_target, 'foreign'))
  raise 'historical candidates failed to recover' unless cleaner.candidates == expected
  reject_removal(cleaner, [recent + '/build/module-supervisor-target'])
  reject_removal(cleaner, [old + '/build/module-supervisor'])
  reject_removal(cleaner, [old + '/build'])
  remove_in_fixture(cleaner, expected)
  raise 'historical caches remain' if expected.any? { |path| File.exist?(File.join(root, path)) }
  raise 'published ARM artifact changed' unless File.read(published) == artifact_data
  raise 'protected source or evidence changed' unless protected.all? { |path| File.exist?(File.join(root, path)) }
  raise 'newer release capsule changed' unless File.directory?(File.join(root, recent, 'build/python-cache'))
end
Dir.mktmpdir('proletarius-clean-direct-cargo-incremental-test') do |root|
  cache = GeneratedCacheCleaner::DIRECT_CARGO_INCREMENTAL
  target = 'src/rust/target'
  write_fixture(root, target + '/CACHEDIR.TAG', GeneratedCacheCleaner::CARGO_CACHE_TAG + "\n")
  write_fixture(root, target + '/.rustc_info.json', '{}')
  write_fixture(root, cache + '/crate-hash/session.lock', '')
  write_fixture(root, cache + '/crate-hash/session/object.o', 'compiler object')
  write_fixture(root, cache + '/crate-hash/session/query-cache.bin', 'compiler cache')
  protected = [target + '/debug/switchd-rs', target + '/debug/deps/test-executable',
               target + '/debug/build/build-script', 'build/reports/evidence.json']
  protected.each { |path| write_fixture(root, path, 'keep') }
  cleaner = GeneratedCacheCleaner.new(root)
  raise 'direct incremental cache missing' unless cleaner.candidates == [cache]
  reject_removal(cleaner, [target])
  reject_removal(cleaner, [target + '/debug'])
  reject_removal(cleaner, [target + '/debug/deps'])

  write_fixture(root, cache + '/crate-hash/session/source.rs', 'retain')
  raise 'accepted source under compiler intermediates' unless cleaner.candidates.empty?
  File.unlink(File.join(root, cache, 'crate-hash/session/source.rs'))
  File.symlink(File.join(root, 'build/reports'), File.join(root, cache, 'foreign'))
  raise 'accepted symlink under compiler intermediates' unless cleaner.candidates.empty?
  File.unlink(File.join(root, cache, 'foreign'))
  write_fixture(root, target + '/.rustc_info.json', '{invalid')
  raise 'accepted invalid Cargo marker' unless cleaner.candidates.empty?
  write_fixture(root, target + '/.rustc_info.json', '{}')
  raise 'direct incremental cache did not recover' unless cleaner.candidates == [cache]
  remove_in_fixture(cleaner, [cache])
  raise 'direct incremental cache remains' if File.exist?(File.join(root, cache))
  raise 'direct Cargo product or evidence changed' unless
    protected.all? { |path| File.read(File.join(root, path)) == 'keep' }
  raise 'direct Cargo target root changed' unless
    File.file?(File.join(root, target, 'CACHEDIR.TAG'))
end
Dir.mktmpdir('proletarius-clean-nested-derived-test') do |root|
  derived = 'build/dmg-build-fixture/DerivedData'
  expected = GeneratedCacheCleaner::CACHE_PATHS.map { |suffix| derived + '/' + suffix }.sort
  expected.each { |path| write_fixture(root, path + '/generated') }
  protected = [derived + '/info.plist', derived + '/Build/Products/application',
               derived + '/Logs/evidence', 'build/dmg-build-fixture/source.tar.gz',
               'outside/keep']
  protected.each { |path| write_fixture(root, path) }

  # The new pattern is exact; arbitrary nested roots and archived source roots
  # cannot become disposable merely because they contain DerivedData metadata.
  ['build/unrecognised/DerivedData',
   'build/dmg-build-fixture/arbitrary/DerivedData',
   'build/dmg-build-source-snapshot/DerivedData'].each do |invalid|
    ['info.plist', 'Build/Intermediates.noindex/keep', 'Build/Products/application',
     'Logs/evidence'].each do |suffix|
      path = invalid + '/' + suffix
      write_fixture(root, path)
      protected << path
    end
  end
  snapshot = 'build/dmg-build-source-snapshot/DerivedData/source.tar.gz'
  write_fixture(root, snapshot)
  protected << snapshot

  # Keep every existing no-symlink boundary, including symlinked ancestors.
  links = []
  parent = File.join(root, 'build/dmg-build-linked-root')
  FileUtils.mkdir_p(parent)
  File.symlink(File.join(root, derived), File.join(parent, 'DerivedData'))
  links << 'build/dmg-build-linked-root/DerivedData'
  File.symlink(File.join(root, 'build/dmg-build-fixture'),
               File.join(root, 'build/dmg-build-linked-parent'))
  links << 'build/dmg-build-linked-parent'
  ['info', 'build', 'cache'].each do |kind|
    alias_root = 'build/dmg-build-linked-' + kind + '/DerivedData'
    write_fixture(root, alias_root + '/info.plist')
    write_fixture(root, alias_root + '/Build/Products/application')
    protected << alias_root + '/Build/Products/application'
    case kind
    when 'info'
      File.unlink(File.join(root, alias_root, 'info.plist'))
      File.symlink(File.join(root, derived, 'info.plist'), File.join(root, alias_root, 'info.plist'))
      links << alias_root + '/info.plist'
    when 'build'
      FileUtils.remove_entry_secure(File.join(root, alias_root, 'Build'))
      File.symlink(File.join(root, derived, 'Build'), File.join(root, alias_root, 'Build'))
      links << alias_root + '/Build'
    when 'cache'
      File.symlink(File.join(root, 'outside'), File.join(root, alias_root, 'ModuleCache.noindex'))
      links << alias_root + '/ModuleCache.noindex'
    end
  end
  File.symlink(File.join(root, 'outside'), File.join(root, expected.first, 'foreign'))
  cleaner = GeneratedCacheCleaner.new(root)
  raise 'nested DerivedData candidate list is unsafe' unless cleaner.candidates == expected
  raise 'nested dry-run changed evidence' unless protected.all? { |path| File.read(File.join(root, path)) == 'keep' }
  [derived, 'build/dmg-build-fixture', derived + '/Build/Products',
   derived + '/Logs', derived + '/info.plist', snapshot].each do |invalid|
    reject_removal(cleaner, [expected.first, invalid])
  end
  raise 'unsafe nested request partially deleted caches' unless expected.all? { |path| File.directory?(File.join(root, path)) }
  remove_in_fixture(cleaner, expected)
  raise 'nested cache not removed' if expected.any? { |path| File.exist?(File.join(root, path)) }
  raise 'nested protected evidence changed' unless protected.all? { |path| File.read(File.join(root, path)) == 'keep' }
  raise 'nested root or cache symlink changed' unless links.all? { |path| File.symlink?(File.join(root, path)) }
  raise 'unexpected nested residual candidates' unless cleaner.candidates.empty?
end
Dir.mktmpdir('proletarius-clean-workspace-incremental-test') do |root|
  target = 'build/rust-target'
  cache = target + '/debug/incremental'
  write_fixture(root, target + '/CACHEDIR.TAG', GeneratedCacheCleaner::CARGO_CACHE_TAG + "\n")
  write_fixture(root, target + '/.rustc_info.json', '{}')
  write_fixture(root, cache + '/crate-hash/session/object.o', 'compiler object')
  write_fixture(root, cache + '/crate-hash/session/work-products.bin', 'compiler map')
  write_fixture(root, cache + '/crate-hash/session.lock', '')
  protected = [target + '/debug/switchd-rs', target + '/debug/deps/current-tests',
               'build/reports/verification.json', 'src/source.rs']
  protected.each { |path| write_fixture(root, path, 'keep') }
  cleaner = GeneratedCacheCleaner.new(root)
  raise 'workspace incremental not individually selectable' unless cleaner.candidates == [target, cache]
  File.unlink(File.join(root, target, 'CACHEDIR.TAG'))
  raise 'pre-created workspace Cargo root lost selective cleanup' unless cleaner.candidates == [target, cache]
  write_fixture(root, target + '/.rustc_info.json', 'invalid')
  raise 'invalid workspace Cargo marker accepted' if cleaner.candidates.include?(cache)
  write_fixture(root, target + '/.rustc_info.json', '{}')
  write_fixture(root, cache + '/unexpected-source.rs', 'keep')
  raise 'workspace source was classified as incremental' if cleaner.candidates.include?(cache)
  File.unlink(File.join(root, cache, 'unexpected-source.rs'))
  File.symlink(File.join(root, 'build/reports'), File.join(root, cache, 'foreign'))
  raise 'workspace linked incremental was accepted' if cleaner.candidates.include?(cache)
  File.unlink(File.join(root, cache, 'foreign'))
  remove_in_fixture(cleaner, [cache])
  raise 'workspace incremental remains' if File.exist?(File.join(root, cache))
  raise 'selective incremental cleanup damaged products or evidence' unless
    protected.all? { |path| File.read(File.join(root, path)) == 'keep' }
end
puts 'PASS: exact allowlist, nested DerivedData, recognised capsules, protected evidence, symlinks and active-build guards'
