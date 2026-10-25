# Reuse the explicit mock Security/transport boundaries; never import Security.
source_root, destination = ARGV
prepare = File.join(source_root, 'app/Tests/SMSForwardingClient/prepare.rb')
abort 'mock preparation failed' unless system('ruby', prepare, File.join(source_root, 'app/Sources'), destination)
text = File.read(File.join(source_root, 'app/Tests/SMSForwardingClient/main.swift'))
marker = '@main @MainActor enum ForwardingClientTests'
abort 'mock boundary marker changed' unless text.scan(marker).length == 1
prefix = text.split(marker, 2).first
abort 'Security unexpectedly imported' if prefix.match?(/^import Security\b/)
File.write(File.join(destination, 'MockBoundaries.swift'), prefix)
