# Compile the complete production store/model with isolated IO adapters. Only
# dependency imports, access control, defaults and subscription wiring change;
# refresh, deletion, generation checks and notification logic stay untouched.
source_dir, output_dir = ARGV
abort 'usage: prepare.rb SOURCE_DIR OUTPUT_DIR' unless source_dir && output_dir

source = File.read(File.join(source_dir, 'SMSStore.swift'))
source = source.gsub(/^import (Combine|AppKit|UserNotifications)\n/, '')
pattern = /        backend\.\$state\n.*?            \.store\(in: &cancellables\)\n/m
abort 'SMS state subscription extraction marker changed' unless source.scan(pattern).length == 1
source = source.sub(pattern, '')
source = source.gsub(/^    private var cancellables = Set<AnyCancellable>\(\)\n/, '')
source = source.gsub('@Published ', '').gsub('private(set) ', '').gsub(/\bprivate /, '')
source = source.gsub('UserDefaults.standard', 'SMSTestDefaults.shared')
File.write(File.join(output_dir, 'SMSStore.swift'), source)
File.write(File.join(output_dir, 'SMSModels.swift'), File.read(File.join(source_dir, 'SMSModels.swift')))
