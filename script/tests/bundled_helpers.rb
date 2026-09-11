require 'minitest/autorun'
require 'tmpdir'
require 'fileutils'
require 'open3'
require 'json'

class BundledHelpers < Minitest::Test
  ROOT = File.expand_path('../..', __dir__)
  BUILD = '26.9.1+source.' + 'a' * 64

  def fixture
    Dir.mktmpdir('agentty-bundled-') do |dir|
      assets = File.join(dir, 'assets with spaces')
      bin = File.join(dir, 'tools')
      FileUtils.mkdir_p([assets, bin])
      native = File.join(dir, 'native server')
      File.write(native, "#!/bin/sh\n[ \"$1\" = --protocol ] || exit 43\nprintf '%s\\n' '{\"build\":\"#{BUILD}\",\"control\":16,\"protocol\":6}'\n")
      FileUtils.chmod(0755, native)
      %w[x86_64 aarch64].each do |arch|
        header = "\x7fELF\x02\x01\x01".b.ljust(64, "\0")
        header[16, 4] = [2, arch == 'x86_64' ? 62 : 183].pack('v2')
        File.binwrite(File.join(assets, "tty7-server-linux-#{arch}-musl"), header + BUILD)
      end
      File.write(File.join(bin, 'file'), "#!/bin/sh\necho 'ELF 64-bit LSB executable, statically linked'\nexit \"${INSPECT_STATUS:-0}\"\n")
      File.write(File.join(bin, 'readelf'), "#!/bin/sh\necho 'LOAD'\nexit \"${INSPECT_STATUS:-0}\"\n")
      FileUtils.chmod(0755, Dir[File.join(bin, '*')])
      run = ->(extra = {}) { Open3.capture3({'PATH' => "#{bin}:#{ENV.fetch('PATH')}", 'INSPECT_STATUS' => '0'}.merge(extra), 'bash', File.join(ROOT, 'script/check_bundled_remote_helpers'), native, assets) }
      yield native, assets, run
    end
  end

  def test_matching_assets_pass_without_executing_linux_files
    fixture do |_, _, run|
      out, err, status = run.call
      assert status.success?, out + err
      assert_includes out, 'not runtime acceptance'
    end
  end

  def test_identity_guard_follows_shared_parser_and_both_consumers
    Dir.mktmpdir('agentty-identity-wiring-') do |dir|
      paths = %w[script/check_build_identity crates/tty7-core/build.rs crates/tty7-core/src/daemon/install/mod.rs .github/scripts/bundle-macos.sh script/macos_bundle_metadata script/lib/server_protocol_identity.rb script/lib/check_bundled_remote_helpers.rb]
      paths.each do |path|
        FileUtils.mkdir_p(File.dirname(File.join(dir, path)))
        FileUtils.cp(File.join(ROOT, path), File.join(dir, path))
      end
      check = File.join(dir, 'script/check_build_identity')
      out, err, status = Open3.capture3('ruby', check)
      assert status.success?, out + err
      {
        'script/macos_bundle_metadata' => 'ServerProtocolIdentity.parse',
        'script/lib/check_bundled_remote_helpers.rb' => 'ServerProtocolIdentity.parse',
        'script/lib/server_protocol_identity.rb' => '[0-9a-f]{64}'
      }.each do |path, token|
        file = File.join(dir, path)
        original = File.read(file)
        mutation = original.sub(token, 'INVALID_FIXTURE_REPLACEMENT')
        refute_equal original, mutation
        File.write(file, mutation)
        _, err, status = Open3.capture3('ruby', check)
        refute status.success?, "#{path} bypass passed"
        assert_includes err, path
        File.write(file, original)
      end
    end
  end

  def test_missing_wrong_architecture_or_stale_asset_fails
    [:missing, :arch, :stale, :conflicting, :symlink].each do |failure|
      fixture do |_, assets, run|
        path = File.join(assets, 'tty7-server-linux-aarch64-musl')
        bytes = File.binread(path)
        case failure
        when :missing then FileUtils.rm(path)
        when :arch
          bytes[18, 2] = [62].pack('v')
          File.binwrite(path, bytes)
        when :stale then File.binwrite(path, bytes.sub(BUILD, BUILD.sub('a' * 64, 'b' * 64)))
        when :conflicting then File.binwrite(path, bytes + '26.9.1+source.' + 'b' * 64)
        when :symlink
          FileUtils.rm(path)
          File.symlink('tty7-server-linux-x86_64-musl', path)
        end
        out, err, status = run.call
        refute status.success?, "#{failure}: #{out}#{err}"
        refute_includes out, 'not runtime acceptance'
        expected = { missing: 'No such file', arch: 'architecture/header mismatch',
                     stale: 'source marker mismatch', conflicting: 'source marker mismatch',
                     symlink: 'non-symlink file' }.fetch(failure)
        assert_includes err, expected
      end
    end
  end

  def test_native_and_inspector_failures_fail_closed
    fixture do |native, _, run|
      _, err, status = run.call('INSPECT_STATUS' => '1')
      refute status.success?
      assert_includes err, 'static ELF inspection failed'
      ["echo '{}'; exit 0", "echo '{}'; exit 1", "echo '{\"build\":\"#{BUILD}\",\"control\":16,\"control\":16,\"protocol\":6}'"].each do |body|
        File.write(native, "#!/bin/sh\n#{body}\n")
        out, err, status = run.call
        refute status.success?, out + err
      end
    end
  end
end
