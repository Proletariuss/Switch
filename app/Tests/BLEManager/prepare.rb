# This adapter generation changes dependency names/access control only. The
# complete production manager's state machine, callbacks and APDU code compile
# into the test binary on every invocation; no copied business logic is used.
source_dir, output_dir = ARGV
abort 'usage: prepare.rb SOURCE_DIR OUTPUT_DIR' unless source_dir && output_dir

def replace_once(source, pattern, replacement)
  count = source.scan(pattern).length
  abort "production extraction marker changed: #{pattern.inspect} (#{count} matches)" unless count == 1
  source.sub(pattern, replacement)
end

manager = File.read(File.join(source_dir, 'BluetoothReaderManager.swift'))
manager = replace_once(manager, /\n\/\/\/ The UI consumes.*\z/m, "\n")
manager = replace_once(manager, /^import Combine\n/, '')
manager = replace_once(manager, /^import CoreBluetooth\n/, '')
# Redirect diagnostic logging to an in-memory test collector; never touch disk.
# ReaderOperationLogger is provided by Adapters.swift.
manager = replace_once(manager, /^    private func bleDiagLog\(_ msg: String\) \{.*?^    \}/m,
                       '    private func bleDiagLog(_ msg: String) { BLETestDiagnostics.write(msg) }')
# Published properties are plain values in this single-threaded event model.
manager = manager.gsub('@Published ', '').gsub('private(set) ', '').gsub(/\bprivate /, '')
# Observe the exact point at which production code publishes a non-nil
# connectedID. A View may react immediately, so state must already be ready.
manager = replace_once(manager, /^    var connectedID: UUID\?$/,
                       '    var connectedID: UUID? { didSet { if connectedID != nil && state != .connected { BLETestDiagnostics.write("connected_published_before_ready") } } }')
manager = manager.gsub('UserDefaults.standard', 'BLETestDefaults.shared')

deadline = File.read(File.join(source_dir, 'BLEPhaseDeadline.swift'))
{ 'BluetoothReaderManager.swift' => manager, 'BLEPhaseDeadline.swift' => deadline }.each do |name, source|
  source = source.gsub(/\bDispatchWorkItem\b/, 'BLETestWork')
                 .gsub(/\bDispatchQueue\b/, 'BLETestQueue')
                 .gsub('.now() +', 'BLETestQueue.main.now +')
  File.write(File.join(output_dir, name), source)
end
