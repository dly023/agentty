require 'minitest/autorun'
require 'tmpdir'
require 'fileutils'
require 'open3'

class StaticHelperContract < Minitest::Test
  ROOT = File.expand_path('../..', __dir__)

  def inspect_fixture(overrides = {})
    Dir.mktmpdir('tty7-static-helper-') do |dir|
      bin = File.join(dir, 'tools')
      FileUtils.mkdir_p(bin)
      File.write(File.join(dir, 'helper with spaces'), 'fixture, never executed')
      File.write(File.join(bin, 'file'), <<~'SH')
        #!/bin/bash
        printf '%s\n' "${FILE_OUTPUT:-ELF 64-bit LSB executable, statically linked}"
        exit "${FILE_STATUS:-0}"
      SH
      File.write(File.join(bin, 'readelf'), <<~'SH')
        #!/bin/bash
        case "$1" in
          -l*) printf '%s\n' "${HEADERS:-LOAD}"; exit "${HEADER_STATUS:-0}" ;;
          -d*) printf '%s\n' "${DYNAMIC:-There is no dynamic section in this file.}"; exit "${DYNAMIC_STATUS:-0}" ;;
          *) exit 2 ;;
        esac
      SH
      FileUtils.chmod(0755, Dir[File.join(bin, '*')])
      env = { 'PATH' => "#{bin}:/usr/bin:/bin", 'FILE_STATUS' => '0',
              'HEADER_STATUS' => '0', 'DYNAMIC_STATUS' => '0',
              'FILE_OUTPUT' => 'ELF 64-bit LSB executable, statically linked',
              'HEADERS' => 'LOAD', 'DYNAMIC' => 'There is no dynamic section in this file.' }
      output, status = Open3.capture2e(env.merge(overrides), '/bin/bash',
                                     File.join(ROOT, '.github/scripts/assert-static.sh'),
                                     File.join(dir, 'helper with spaces'))
      [status.success?, output]
    end
  end

  def test_static_and_static_pie_are_accepted
    ['statically linked', 'static-pie linked'].each do |kind|
      ok, output = inspect_fixture('FILE_OUTPUT' => "ELF 64-bit LSB executable, #{kind}")
      assert ok, output
    end
  end

  def test_inspector_failures_never_pass_even_with_plausible_output
    %w[FILE_STATUS HEADER_STATUS DYNAMIC_STATUS].each do |key|
      ok, output = inspect_fixture(key => '1')
      refute ok, "#{key} was ignored: #{output}"
    end
  end

  def test_interpreter_dependency_and_non_elf_are_rejected
    [
      { 'HEADERS' => '  INTERP  0x000001 0x000001 0x000001' },
      { 'DYNAMIC' => ' 0x0000000000000001 (NEEDED) Shared library: [libc.so.6]' },
      { 'FILE_OUTPUT' => 'ELF 64-bit LSB executable, dynamically linked' },
      { 'FILE_OUTPUT' => 'ASCII text, statically linked' }
    ].each do |overrides|
      ok, output = inspect_fixture(overrides)
      refute ok, "#{overrides} was accepted: #{output}"
    end
  end

  def test_missing_input_and_wrong_argument_count_fail
    [[], ['/nonexistent/tty7-static-fixture'], ['one', 'two']].each do |args|
      _, status = Open3.capture2e('/bin/bash', File.join(ROOT, '.github/scripts/assert-static.sh'), *args)
      refute status.success?
    end
  end
end
