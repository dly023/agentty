require_relative 'server_protocol_identity'

module VersionPolicy
  def self.validate(metadata, root)
    raise ArgumentError, 'metadata must be an object' unless metadata.is_a?(Hash)
    raise ArgumentError, 'foreign workspace root' unless metadata.fetch('workspace_root') == root
    members = metadata.fetch('workspace_members')
    packages = metadata.fetch('packages')
    unless members.is_a?(Array) && !members.empty? && members.all? { |id| id.is_a?(String) } &&
           members.uniq == members && packages.is_a?(Array) && packages.all? { |p| p.is_a?(Hash) }
      raise ArgumentError, 'invalid workspace members or packages'
    end
    by_id = {}
    packages.each do |package|
      id = package.fetch('id')
      raise ArgumentError, 'duplicate or invalid package identity' unless id.is_a?(String) && !by_id.key?(id)
      by_id[id] = package
    end
    resolved = members.map { |id| by_id.fetch(id) }
    roots = resolved.select { |p| p.fetch('manifest_path') == File.join(root, 'Cargo.toml') }
    raise ArgumentError, 'expected exactly one root package' unless roots.length == 1
    version = roots.first.fetch('version')
    raise ArgumentError, 'invalid root version' unless version.is_a?(String)
    ServerProtocolIdentity.numeric_version(version)
    unless resolved.all? { |p| p.fetch('version') == version }
      raise ArgumentError, 'workspace package versions differ from root'
    end
    version
  end
end
