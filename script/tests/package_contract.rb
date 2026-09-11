require 'minitest/autorun'
require 'open3'
require 'tmpdir'
require 'fileutils'
load File.expand_path('../check_package_cleanup', __dir__)
class PackageContractTest < Minitest::Test
  ROOT = File.expand_path('../..', __dir__)
  def test_package_sources_match_core_and_cleanup_contract
    assert_empty PackageContract.errors(ROOT)
  end
  def test_target_validation_rejects_mismatches_and_path_inputs
    validator = File.join(ROOT, 'script/check_package_target')
    [['aarch64-apple-darwin', 'arm64', 'macos'], ['x86_64-unknown-linux-gnu', 'x86_64', 'linux'], ['x86_64-pc-windows-msvc', 'x86_64', 'windows']].each do |args|
      out, err, status = Open3.capture3('bash', validator, *args)
      assert status.success?, out + err
    end
    [['aarch64-apple-darwin', '../keep', 'macos'], ['x86_64-unknown-linux-gnu', 'arm64', 'linux'], ['x86_64-pc-windows-msvc', '', 'windows'], ['x86_64-unknown-linux-gnu', 'x86_64', 'unknown']].each do |args|
      _, _, status = Open3.capture3('bash', validator, *args)
      refute status.success?, args.inspect
    end
  end

  def test_failed_package_cleans_only_selected_architecture_and_format
    [
      ['macos', 'aarch64-apple-darwin', 'arm64', 'macos', 'dmg', 'macos-arm64/Agentty.app'],
      ['linux', 'x86_64-unknown-linux-gnu', 'x86_64', 'linux', 'tar.gz', 'agentty-1.0.0-linux-x86_64'],
      ['appimage', 'x86_64-unknown-linux-gnu', 'x86_64', 'linux', 'AppImage', 'AppDir-x86_64']
    ].each do |entry, target, arch, platform, format, stage|
      Dir.mktmpdir('agentty-package-failure-') do |root|
        FileUtils.mkdir_p(File.join(root, 'script'))
        FileUtils.cp(File.join(ROOT, 'script/check_package_target'), File.join(root, 'script'))
        File.write(File.join(root, 'Cargo.toml'), "[workspace.package]\nversion = \"2.0.0\"\n")
        FileUtils.mkdir_p(File.join(root, 'dist', stage))
        File.write(File.join(root, 'dist', stage, 'stale'), 'old stage')
        old = "agentty-1.0.0-#{platform}-#{arch}.#{format}"
        other_arch = "agentty-1.0.0-#{platform}-#{arch == 'arm64' ? 'x86_64' : 'arm64'}.#{format}"
        other_format = "agentty-1.0.0-#{platform}-#{arch}.#{format == 'tar.gz' ? 'AppImage' : 'tar.gz'}"
        [old, other_arch, other_format].each { |name| File.write(File.join(root, 'dist', name), name) }
        # No release binaries, helpers or signing credentials exist in the fixture.
        _, _, status = Open3.capture3(
          'bash', File.join(ROOT, ".github/scripts/bundle-#{entry}.sh"), target, arch, chdir: root
        )
        refute status.success?, 'missing new inputs must fail'
        refute File.exist?(File.join(root, 'dist', old)), "#{entry}: stale final survived"
        refute File.exist?(File.join(root, 'dist', stage)), "#{entry}: stale stage survived"
        assert File.exist?(File.join(root, 'dist', other_arch)), "#{entry}: other architecture removed"
        assert File.exist?(File.join(root, 'dist', other_format)), "#{entry}: sibling format removed"
      end
    end
  end
end
