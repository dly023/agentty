require 'json'

module ServerProtocolIdentity
  class UniqueObject < Hash
    def []=(key, value)
      raise ArgumentError, 'duplicate protocol field' if key?(key)
      super
    end
  end

  def self.numeric_version(version)
    match = /\A(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?(?:\+([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?\z/.match(version)
    raise ArgumentError, 'invalid Cargo SemVer' unless match
    if match[4]&.split('.')&.any? { |part| part.match?(/\A0[0-9]+\z/) }
      raise ArgumentError, 'noncanonical numeric prerelease identifier'
    end
    match.captures.first(3).join('.')
  end

  def self.parse(reply, version: nil)
    protocol = JSON.parse(reply, object_class: UniqueObject)
    unless protocol.is_a?(Hash) && protocol.keys.sort == %w[build control protocol] &&
           %w[control protocol].all? { |key| protocol[key].is_a?(Integer) && (0..0xffff_ffff).cover?(protocol[key]) }
      raise ArgumentError, 'invalid native protocol fields'
    end
    identity = protocol['build']
    match = identity.is_a?(String) && /\A(.+)\+source\.([0-9a-f]{64})\z/.match(identity)
    raise ArgumentError, 'invalid native source identity' unless match
    numeric_version(match[1])
    if version && match[1] != version
      raise ArgumentError, 'native source identity does not match release version'
    end
    protocol
  end
end
