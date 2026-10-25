#!/usr/bin/env ruby
# Compile the production manual-selection policy without launching SwiftUI or
# CoreBluetooth. Fail if the View stops wiring it to explicit Connect events.
source_path, output_path = ARGV
abort 'usage: prepare.rb SOURCE OUTPUT' unless source_path && output_path
source = File.read(source_path)
sheet = source.split('private struct ESTKBluetoothSheet: View {', 2).last
abort 'Bluetooth sheet missing' unless sheet
abort 'manual Connect no longer records the requested device' unless sheet.include?('manualSelection.request(id)')
abort 'already-connected BLE reader no longer has an explicit Use action' unless sheet.include?('manualSelection.useConnected(id, blockReason: selectionBlockReason())')
abort 'GATT completion no longer resolves manual selection' unless sheet.include?('manualSelection.connected(id, blockReason: selectionBlockReason())')
abort 'BLE disconnection must not directly disconnect the selected USB reader' if sheet.include?('store.disconnectReader()')
abort 'manual BLE selection no longer connects the backend reader' unless source.include?('store.connectReader(reader)')
abort 'BLE discovery/recovery must not auto-select a reader' unless source.include?('store.refreshReaderList(autoConnect: false)')
abort 'reader picker and other-reader switch must share the operation guard' unless source.scan('if let reason = bluetoothReaderSwitchBlockReason').length == 2

def declaration(source, signature)
  start = source.index(signature) or abort "production declaration missing: #{signature}"
  brace = source.index('{', start)
  depth = 1
  finish = brace + 1
  while depth.positive? && finish < source.length
    depth += 1 if source[finish] == '{'
    depth -= 1 if source[finish] == '}'
    finish += 1
  end
  abort "unbalanced production declaration: #{signature}" unless depth.zero?
  source[start...finish]
end

policy = declaration(source, 'struct ReaderBLEManualSelection {')
guard = declaration(source, 'private var bluetoothReaderSwitchBlockReason:')
File.write(output_path, <<~SWIFT)
  import Foundation

  #{policy}

  struct ReaderBLEGuardStatus {
      var tunnelState: String
      var imsState: String?
  }

  enum ReaderBLEGuardLoadPhase { case idle, loading }

  struct ReaderBLEGuardStore {
      var readerActionInFlight = false
      var readerVoWiFiActionInFlight = false
      var readerConnected = false
      var readerName: String?
      var readerVoWiFiLoaded = false
      var readerVoWiFiStatus: ReaderBLEGuardStatus?
  }

  struct ReaderBLEGuardESIMStore {
      var isBusy = false
      var loadPhase: ReaderBLEGuardLoadPhase = .idle
  }

  struct ReaderBLEGuardFixture {
      var store = ReaderBLEGuardStore()
      var readerESIMStore = ReaderBLEGuardESIMStore()

      #{guard.sub('private var ', 'var ')}
  }
SWIFT
