#!/usr/bin/env ruby
require 'minitest/autorun'
require 'tmpdir'
require 'fileutils'
require 'open3'
require 'json'

class VersionPolicyTest < Minitest::Test
  ROOT = File.expand_path('../..', __dir__)
  CHECK = File.join(ROOT, 'script/check_version_policy')

  def metadata(root, version = '26.9.1')
    {'workspace_root' => root, 'workspace_members' => ['gui', 'core'], 'packages' => [
      {'id' => 'gui', 'manifest_path' => File.join(root, 'Cargo.toml'), 'version' => version},
      {'id' => 'core', 'manifest_path' => File.join(root, 'crates/core/Cargo.toml'), 'version' => version}
    ]}
  end

  def run_fixture
    Dir.mktmpdir('agentty-version-') do |root|
      root = File.realpath(root)
      FileUtils.mkdir_p(File.join(root, 'bin'))
      shim = File.join(root, 'bin/rustup')
      File.write(shim, "#!/bin/sh\n" +
        "printf '%s\\n' \"$@\" > \"$VERSION_ARGS\"\n" +
        "cat \"$VERSION_METADATA\"\nexit \"$VERSION_EXIT\"\n")
      FileUtils.chmod(0755, shim)
      path = File.join(root, 'metadata.json')
      args = File.join(root, 'args')
      env = {'PATH' => File.join(root, 'bin') + ':' + ENV.fetch('PATH'),
             'VERSION_METADATA' => path, 'VERSION_ARGS' => args, 'VERSION_EXIT' => '0'}
      invoke = lambda do |data, exit_code = '0'|
        File.write(path, data.is_a?(String) ? data : JSON.generate(data))
        Open3.capture3(env.merge('VERSION_EXIT' => exit_code), CHECK, root)
      end
      yield root, invoke, args
    end
  end

  def test_current_and_future_versions_use_the_same_read_only_command
    run_fixture do |root, invoke, args|
      %w[26.9.1 27.1.0-rc.1+build.2].each do |version|
        out, err, status = invoke.call(metadata(root, version))
        assert status.success?, out + err
        assert_includes out, version
        assert_equal ['run', 'nightly-2026-07-31', 'cargo', 'metadata', '--format-version', '1',
                      '--no-deps', '--locked', '--offline', '--manifest-path', File.join(root, 'Cargo.toml')],
                     File.readlines(args, chomp: true)
      end
    end
  end

  def test_invalid_workspace_metadata_fails_closed
    run_fixture do |root, invoke, _|
      mutations = [
        ->(d) { d['packages'][1]['version'] = '26.9.2' },
        ->(d) { d['packages'].each { |p| p['version'] = '01.2.3' } },
        ->(d) { d['packages'].each { |p| p['version'] = '1.2.3-01' } },
        ->(d) { d['packages'].shift },
        ->(d) { d['packages'].pop },
        ->(d) { d['packages'] << d['packages'][0].dup },
        ->(d) { d['workspace_members'] << 'gui' },
        ->(d) { d['workspace_members'] = [] },
        ->(d) { d['workspace_root'] = '/foreign' }
      ]
      mutations.each do |mutate|
        data = metadata(root)
        mutate.call(data)
        out, err, status = invoke.call(data)
        refute status.success?, data.inspect
        assert_includes err, 'version policy:'
        refute_includes out, 'version policy passed'
      end
      ['{', '{}', 'null'].each do |data|
        _, err, status = invoke.call(data)
        refute status.success?
        assert_includes err, 'version policy:'
      end
      _, err, status = invoke.call(metadata(root), '7')
      refute status.success?
      assert_includes err, 'Cargo metadata failed'
    end
  end
end
