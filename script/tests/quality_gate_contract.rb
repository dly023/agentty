#!/usr/bin/env ruby
require 'minitest/autorun'
require 'tmpdir'
require 'fileutils'
require 'open3'
require 'yaml'

class QualityGateContract < Minitest::Test
  ROOT = File.expand_path('../..', __dir__)
  CHECK = File.join(ROOT, 'script/check_agent_harness')

  def fixture
    Dir.mktmpdir('agentty-quality-') do |root|
      %w[docs/specs docs/quality script src crates].each { |p| FileUtils.mkdir_p(File.join(root, p)) }
      File.write(File.join(root, 'docs/specs/test.yaml'), {'contracts' => [{'id' => 'TEST-01'}]}.to_yaml)
      File.write(File.join(root, 'docs/quality/traceability.yaml'), {'requirements' => [{
        'id' => 'TEST-01', 'source' => ['src/test.rs'], 'static_check' => 'script/check_test',
        'tests' => ['proves_behavior']
      }]}.to_yaml)
      File.write(File.join(root, 'src/test.rs'), "#[test]\nfn proves_behavior() {}\n")
      File.write(File.join(root, 'script/check_test'), "#!/bin/sh\nexit 0\n")
      File.write(File.join(root, 'AGENTS.md'), "# Engineering contract\n")
      File.write(File.join(root, 'DEVELOPMENT.md'), "# Development\n[Rules](AGENTS.md)\n")
      File.write(File.join(root, 'docs/README.md'), "# Engineering index\n[Development](../DEVELOPMENT.md)\n")
      yield root
    end
  end

  def run_check(root)
    Open3.capture3('ruby', CHECK, root)
  end

  def test_valid_references_pass
    fixture do |root|
      out, err, status = run_check(root)
      assert status.success?, out + err
    end
  end

  def test_navigation_requires_current_nonempty_entries
    %w[AGENTS.md DEVELOPMENT.md docs/README.md].each do |entry|
      [:missing, :empty, :foreign, :directory].each do |condition|
        fixture do |root|
          path = File.join(root, entry)
          case condition
          when :missing then FileUtils.rm(path)
          when :empty then File.write(path, " \n")
          when :foreign
            FileUtils.rm(path)
            File.symlink(File.join(ROOT, 'Cargo.toml'), path)
          when :directory
            FileUtils.rm(path)
            FileUtils.mkdir_p(path)
          end
          _, err, status = run_check(root)
          refute status.success?, "#{condition} #{entry} must fail"
          assert_includes err, entry
        end
      end
    end
  end

  def test_navigation_rejects_broken_and_foreign_links
    fixture do |root|
      %w[DEVELOPMENT.md docs/README.md].each do |entry|
        path = File.join(root, entry)
        original = File.read(path)
        ['missing-current-spec.md', '/etc/hosts'].each do |target|
          File.write(path, original + "\n[Invalid](#{target})\n")
          _, err, status = run_check(root)
          refute status.success?, "#{entry} must reject #{target}"
          assert_includes err, entry
          assert_includes err, target
        end
        File.write(path, original)
      end
      File.symlink('/etc/hosts', File.join(root, 'foreign.md'))
      File.write(File.join(root, 'DEVELOPMENT.md'), "[Foreign](foreign.md)\n")
      _, err, status = run_check(root)
      refute status.success?
      assert_includes err, 'foreign.md'
    end
  end

  def test_host_boundary_excludes_test_modules_without_exempting_production
    fixture do |root|
      FileUtils.mkdir_p(File.join(root, '.github/scripts'))
      FileUtils.mkdir_p(File.join(root, 'src/ui'))
      FileUtils.mkdir_p(File.join(root, 'src/terminal'))
      check = File.join(root, '.github/scripts/check-host-boundary.sh')
      FileUtils.cp(File.join(ROOT, '.github/scripts/check-host-boundary.sh'), check)
      10.times { |i| File.write(File.join(root, "src/ui/fixture_#{i}.rs"), '// fixture') }
      source = File.join(root, 'src/ui/fixture_0.rs')
      ['', 'pub ', 'pub(crate) '].each do |visibility|
        test_body = "#[cfg(test)]\n#{visibility}mod tests {\nfn fixture() { std::fs::remove_file(\"temp\"); }\n}\n"
        File.write(source, test_body)
        out, err, status = Open3.capture3('bash', check)
        assert status.success?, "test visibility #{visibility.inspect}: #{out}#{err}"
        File.write(source, "fn production() { std::fs::read(\"target\"); }\n" + test_body)
        out, err, status = Open3.capture3('bash', check)
        refute status.success?, "production must remain forbidden: #{out}#{err}"
        assert_includes err, 'fixture_0.rs:1:'
        File.write(source, "#{visibility}mod runtime {\nfn access() { std::fs::read(\"target\"); }\n}\n")
        out, err, status = Open3.capture3('bash', check)
        refute status.success?, "visibility is not test scope: #{out}#{err}"
        assert_includes err, 'fixture_0.rs:2:'
      end
    end
  end

  def test_missing_source_check_and_archive_only_test_fail
    ['src/test.rs', 'script/check_test'].each do |missing|
      fixture do |root|
        FileUtils.rm(File.join(root, missing))
        _, err, status = run_check(root)
        refute status.success?
        assert_includes err, missing
      end
    end
    fixture do |root|
      FileUtils.mkdir_p(File.join(root, '历史归档/src'))
      FileUtils.mv(File.join(root, 'src/test.rs'), File.join(root, '历史归档/src/test.rs'))
      File.write(File.join(root, 'src/test.rs'), '// no current test')
      _, err, status = run_check(root)
      refute status.success?
      assert_includes err, 'proves_behavior'
    end
  end

  def test_unknown_duplicate_and_malformed_contracts_fail
    ["contracts:\n  - id: OTHER\n", "contracts:\n  - id: TEST-01\n  - id: TEST-01\n", 'contracts: ['].each do |bad|
      fixture do |root|
        File.write(File.join(root, 'docs/specs/test.yaml'), bad)
        _, err, status = run_check(root)
        refute status.success?
        refute_empty err
      end
    end
  end

  def test_presubmit_rejects_bad_mode_without_running_checks
    _, err, status = Open3.capture3('bash', File.join(ROOT, 'script/presubmit'), 'unknown')
    assert_equal 2, status.exitstatus
    assert_includes err, 'usage:'
  end

  def test_presubmit_propagates_check_failure_without_success_banner
    fixture do |root|
      FileUtils.cp(File.join(ROOT, 'script/presubmit'), File.join(root, 'script/presubmit'))
      File.write(File.join(root, 'script/check_agent_harness'), "warn 'fixture check failed'; exit 43\n")
      out, err, status = Open3.capture3('bash', File.join(root, 'script/presubmit'), 'full', :chdir => '/')
      assert_equal 43, status.exitstatus
      assert_includes err, 'fixture check failed'
      refute_includes out, 'passed'
    end
  end

  def test_full_runs_locked_workspace_tests_and_format_failure_stops_it
    [0, 42].each do |format_status|
      fixture do |root|
        %w[script/tests .github/scripts bin].each { |p| FileUtils.mkdir_p(File.join(root, p)) }
        FileUtils.cp(File.join(ROOT, 'script/presubmit'), File.join(root, 'script/presubmit'))
        File.write(File.join(root, 'script/check_agent_harness'), "exit 0\n")
        File.write(File.join(root, 'script/tests/quality_gate_contract.rb'), "exit 0\n")
        %w[check_machine_rail_contract check_machine_preferences check_history_manager check_product_branding check_terminal_bottom check_client_credentials check_path_drag].each do |name|
          File.write(File.join(root, 'script', name), "exit 0\n")
        end
        File.write(File.join(root, '.github/scripts/check-host-boundary.sh'), "exit 0\n")
        shim = File.join(root, 'bin/rustup')
        File.write(shim, "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$AGENTTY_TEST_CARGO_LOG\"\ncase \"$*\" in *'fmt --check') exit #{format_status};; esac\nexit 0\n")
        FileUtils.chmod(0755, shim)
        log = File.join(root, 'cargo.log')
        env = {'PATH' => File.join(root, 'bin') + File::PATH_SEPARATOR + ENV.fetch('PATH'), 'AGENTTY_TEST_CARGO_LOG' => log}
        out, err, status = Open3.capture3(env, 'bash', File.join(root, 'script/presubmit'), 'full', :chdir => '/')
        assert_equal format_status, status.exitstatus, err
        commands = File.readlines(log, :chomp => true)
        assert_equal 'run nightly-2026-07-31 cargo fmt --check', commands.first
        if format_status.zero?
          assert_equal ['run nightly-2026-07-31 cargo fmt --check', 'run nightly-2026-07-31 cargo test --workspace --locked -- --test-threads=1'], commands
          assert_includes out, 'presubmit (full) passed'
        else
          assert_equal 1, commands.length
          refute_includes out, 'passed'
        end
      end
    end
  end
end
