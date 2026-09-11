require 'minitest/autorun'
require 'tmpdir'
require 'fileutils'
require 'open3'
require 'json'
load File.expand_path('../check_ci_lineage', __dir__)

class CiLineageTest < Minitest::Test
  ROOT = File.expand_path('../..', __dir__)
  def with_checkout
    Dir.mktmpdir('agentty-ci-lineage-') do |root|
      %w[Cargo.toml .cargo/config.toml .github/workflows/ci.yml .github/workflows/release.yml .github/workflows/nightly.yml crates/tty7-server/Cargo.toml crates/tty7-core/src/daemon/install/asset.rs].each do |relative|
        dest = File.join(root, relative)
        FileUtils.mkdir_p(File.dirname(dest))
        FileUtils.cp(File.join(ROOT, relative), dest)
      end
      yield root
    end
  end

  def exercise_app_alias(fail_build: false)
    Dir.mktmpdir('agentty-app-alias-') do |root|
      fake = File.join(root, 'cargo')
      calls = File.join(root, 'calls.jsonl')
      File.write(fake, <<~'RUBY')
        #!/usr/bin/env ruby
        require 'json'
        File.open(ENV.fetch('ALIAS_TEST_CALLS'), 'a') { |f| f.puts JSON.generate(ARGV) }
        exit 23 if ENV['ALIAS_TEST_FAIL_BUILD'] == '1' && ARGV.first == 'build'
      RUBY
      File.chmod(0o755, fake)
      command = File.read(File.join(ROOT, '.cargo/config.toml')).match(/^app = "!([^"]+)"$/)[1]
      out, err, status = Open3.capture3(
        {'PATH' => "#{root}:#{ENV.fetch('PATH')}", 'ALIAS_TEST_CALLS' => calls,
         'ALIAS_TEST_FAIL_BUILD' => fail_build ? '1' : '0'},
        'sh', '-c', command + ' "$@"', 'app', '--config-dir', '/isolated/path with spaces',
        chdir: root
      )
      yield File.readlines(calls).map { |line| JSON.parse(line) }, status, out + err
    end
  end

  def test_app_alias_executes_core_names_and_forwards_arguments
    exercise_app_alias do |calls, status, output|
      assert status.success?, output
      assert_equal [
        %w[build -p tty7-server --locked],
        ['run', '--bin', 'tty7-app', '--locked', '--', '--config-dir', '/isolated/path with spaces']
      ], calls
    end
  end

  def test_app_alias_does_not_launch_after_build_failure
    exercise_app_alias(fail_build: true) do |calls, status, _|
      assert_equal 23, status.exitstatus
      assert_equal [%w[build -p tty7-server --locked]], calls
    end
  end

  def test_wrong_app_alias_is_rejected
    with_checkout do |root|
      path = File.join(root, '.cargo/config.toml')
      original = File.read(path)
      [original.gsub('tty7-server', 'missing-server'),
       original.gsub('tty7-app', 'missing-app'),
       original.gsub('--locked', ''),
       original.gsub('&&', ';')].each do |invalid|
        File.write(path, invalid)
        assert CiLineage.errors(root).any? { |error| error.start_with?('development:') }
      end
      FileUtils.rm(path)
      assert CiLineage.errors(root).any? { |error| error.include?('cannot validate inputs') }
    end
  end
  def test_current_workflows_use_real_core_outputs
    assert_empty CiLineage.errors(ROOT)
  end

  def test_wrong_package_and_asset_are_rejected
    with_checkout do |root|
      path = File.join(root, '.github/workflows/ci.yml')
      File.write(path, File.read(path).gsub('tty7-server', 'missing-server'))
      errors = CiLineage.errors(root).join("\n")
      assert_includes errors, 'ci: server build must select tty7-server'
      assert_includes errors, 'ci: missing runtime asset tty7-server-linux-x86_64-musl'
      FileUtils.rm(File.join(root, '.github/workflows/nightly.yml'))
      assert CiLineage.errors(root).any? { |error| error.include?('cannot validate inputs') }, 'missing workflow must fail closed'
    end
  end
end
