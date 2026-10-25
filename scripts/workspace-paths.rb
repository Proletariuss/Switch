# Shared temporary/cache boundary for directly invoked Ruby tools.
require 'fileutils'
SWITCH_WORKSPACE = File.expand_path('../..', __dir__) unless defined?(SWITCH_WORKSPACE)
ENV['TMPDIR'] = File.join(SWITCH_WORKSPACE, 'build', 'tmp')
ENV['SWITCH_DATA_DIR'] = File.join(SWITCH_WORKSPACE, 'build', 'runtime')
FileUtils.mkdir_p([ENV['TMPDIR'], ENV['SWITCH_DATA_DIR']])
