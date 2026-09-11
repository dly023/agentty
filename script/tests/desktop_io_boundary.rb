#!/usr/bin/env ruby
require 'minitest/autorun'
class DesktopIoBoundary < Minitest::Test
  ROOT = File.expand_path('../..', __dir__)
  def test_gui_consumers_have_no_local_filesystem_implementation
    %w[src/ui/file_copy.rs src/ui/code_editor.rs].each do |path|
      production = File.read(File.join(ROOT, path)).split('#[cfg(test)]', 2).first
      refute_match(/std::fs::/, production, "#{path} must use the client-owned boundary")
      assert_includes production, 'desktop_files::'
    end
  end
end
