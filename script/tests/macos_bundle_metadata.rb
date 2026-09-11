require 'minitest/autorun'
require 'open3'
require 'json'
require 'rexml/document'
require 'tmpdir'
require 'fileutils'
require 'shellwords'

class MacosBundleMetadataTest < Minitest::Test
  ROOT = File.expand_path('../..', __dir__)
  HASH = 'a1' * 32

  def generate(version, reply = nil)
    reply ||= { control: 13, protocol: 4, build: "#{version}+source.#{HASH}" }.to_json
    Open3.capture3('ruby', File.join(ROOT, 'script/macos_bundle_metadata'), version, stdin_data: reply)
  end

  def test_versions_and_source_identity_are_separate_in_actual_plist
    %w[26.9.1 0.0.1 26.9.1-nightly.20260910 26.9.1-rc.1+local.42].each do |version|
      xml, error, status = generate(version)
      assert status.success?, error
      elements = REXML::Document.new(xml).elements['plist/dict'].elements.to_a
      keys = elements.each_slice(2).to_h { |key, value| [key.text, value.text || value.name] }
      assert_equal version.split(/[-+]/).first, keys['CFBundleVersion']
      assert_equal keys['CFBundleVersion'], keys['CFBundleShortVersionString']
      assert_equal version, keys['TTY7ReleaseVersion']
      assert_equal "#{version}+source.#{HASH}", keys['TTY7SourceIdentity']
      assert_equal 'tty7-app', keys['CFBundleExecutable']
      assert_equal 'com.dly023.agentty', keys['CFBundleIdentifier']
      assert_equal 'Agentty', keys['CFBundleDisplayName']
      assert_equal 'agentty', keys['CFBundleIconFile']
      assert_equal 'true', keys['NSHighResolutionCapable']
      assert_equal 'NSApplication', keys['NSPrincipalClass']
      if RUBY_PLATFORM.include?('darwin')
        Dir.mktmpdir('tty7-plist-lint-') do |dir|
          path = File.join(dir, 'Info.plist')
          File.write(path, xml)
          output, lint = Open3.capture2e('/usr/bin/plutil', '-lint', path)
          assert lint.success?, output
        end
      end
    end
  end

  def test_invalid_versions_fail_without_plist_output
    ['', '1.2', '01.2.3', '1.2.3garbage', '1.2.3-01', '1.2.3-rc..1',
     '1.2.3+', '1.2.3+bad&xml', "1.2.3\n"].each do |version|
      xml, _, status = generate(version)
      refute status.success?, version.inspect
      assert_empty xml
    end
  end

  def test_invalid_protocol_reply_fails_without_plist_output
    valid = { 'control' => 13, 'protocol' => 4, 'build' => "26.9.1+source.#{HASH}" }
    replies = ['not JSON', '{}', '[]', 'null', valid.merge('control' => -1).to_json,
               valid.merge('protocol' => 2**32).to_json, valid.merge('protocol' => '4').to_json,
               valid.merge('control' => true).to_json, valid.merge('extra' => 1).to_json,
               valid.merge('build' => "26x9x1+source.#{HASH}").to_json,
               valid.merge('build' => "26.9.1+source.#{HASH.upcase}").to_json,
               valid.merge('build' => "26.9.1+source.#{HASH[0...16]}").to_json,
               valid.to_json.sub('"control":13', '"control":1,"control":13')]
    replies.each do |reply|
      xml, _, status = generate('26.9.1', reply)
      refute status.success?, reply
      assert_empty xml
    end
  end

  def test_real_bundler_writes_numeric_metadata_before_mock_signing
    Dir.mktmpdir('tty7-macos-stage-') do |dir|
      %w[script/lib tools target/aarch64-apple-darwin/release assets/completions bundled-server].each do |path|
        FileUtils.mkdir_p(File.join(dir, path))
      end
      %w[check_package_target macos_bundle_metadata lib/server_protocol_identity.rb].each do |name|
        source = File.join(ROOT, 'script', name)
        FileUtils.cp(source, File.join(dir, 'script', name)) if File.exist?(source)
      end
      # Isolate metadata/staging only. Helper verification is NOT covered
      # by this stub, and no actual codesign, credential access or DMG occurs.
      File.write(File.join(dir, 'script/check_bundled_remote_helpers'), "exit 0\n")
      File.write(File.join(dir, 'tools/codesign'), "#!/bin/bash\nexit 93\n")
      FileUtils.chmod(0755, File.join(dir, 'tools/codesign'))
      File.write(File.join(dir, 'Cargo.toml'), "[workspace.package]\nversion = \"26.9.1\"\n")
      reply = { control: 13, protocol: 4, build: "26.9.1+source.#{HASH}" }.to_json
      server = File.join(dir, 'target/aarch64-apple-darwin/release/tty7-server')
      File.write(server, "#!/bin/bash\nprintf '%s\\n' #{Shellwords.escape(reply)}\n")
      FileUtils.chmod(0755, server)
      %w[tty7-app tty7].each { |name| File.write(File.join(File.dirname(server), name), 'unexecuted payload fixture') }
      %w[assets/tty7.icns assets/completions/test.json bundled-server/tty7-server-linux-x86_64-musl bundled-server/tty7-server-linux-aarch64-musl].each do |path|
        File.write(File.join(dir, path), 'payload fixture')
      end
      output, status = Open3.capture2e(
        { 'PATH' => "#{dir}/tools:/usr/bin:/bin", 'APPLE_SIGNING_IDENTITY' => '', 'APPLE_CERTIFICATE' => '' },
        '/bin/bash', File.join(ROOT, '.github/scripts/bundle-macos.sh'), 'aarch64-apple-darwin', 'arm64', chdir: dir)
      assert_equal 93, status.exitstatus, output
      doc = REXML::Document.new(File.read(File.join(dir, 'dist/macos-arm64/Agentty.app/Contents/Info.plist')))
      fields = doc.elements['plist/dict'].elements.to_a.each_slice(2).to_h { |key, value| [key.text, value.text] }
      assert_equal '26.9.1', fields['CFBundleVersion']
      assert_equal reply.then { |json| JSON.parse(json)['build'] }, fields['TTY7SourceIdentity']
    end
  end
end
