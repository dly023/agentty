require 'open3'
require_relative 'server_protocol_identity'

abort 'usage: check_bundled_remote_helpers <native-server> <helper-directory>' unless ARGV.length == 2
native, directory = ARGV.map { |path| File.expand_path(path) }
root = File.expand_path('../..', __dir__)

begin
  regular = lambda do |path|
    raise ArgumentError, "not a regular non-symlink file: #{path}" unless File.lstat(path).file?
  end
  regular.call(native)
  raise ArgumentError, 'native server is not executable' unless File.executable?(native)
  reply, _, status = Open3.capture3(native, '--protocol')
  raise ArgumentError, 'native protocol command failed' unless status.success?
  protocol = ServerProtocolIdentity.parse(reply)
  build = protocol.fetch('build')
  expected_hash = build.split('+source.').last
  { 'x86_64' => 62, 'aarch64' => 183 }.each do |arch, machine|
    path = File.join(directory, "tty7-server-linux-#{arch}-musl")
    regular.call(path)
    raise ArgumentError, "invalid helper size: #{arch}" unless (64..256 * 1024 * 1024).cover?(File.size(path))
    bytes = File.binread(path)
    unless bytes.start_with?("\x7fELF\x02\x01\x01".b) &&
           [2, 3].include?(bytes.byteslice(16, 2).unpack1('v')) &&
           bytes.byteslice(18, 2).unpack1('v') == machine
      raise ArgumentError, "helper architecture/header mismatch: #{arch}"
    end
    _, _, inspected = Open3.capture3('bash', File.join(root, '.github/scripts/assert-static.sh'), path)
    raise ArgumentError, "static ELF inspection failed: #{arch}" unless inspected.success?
    hashes = bytes.scan(/\+source\.([0-9a-f]{64})/n).flatten.uniq
    unless bytes.include?(build.b) && hashes == [expected_hash]
      raise ArgumentError, "embedded source marker mismatch: #{arch}"
    end
  end
  puts 'bundled helper static markers verified; not runtime acceptance or artifact attestation'
rescue SystemCallError, ArgumentError, JSON::ParserError => error
  warn "bundled helpers: #{error.message}"
  exit 1
end
