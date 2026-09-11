//! Internal file paths retain their source machine across UI drag targets.
use std::path::{Path, PathBuf};
use tty7_core::host::HostId;

#[derive(Clone)]
pub struct TreePathDrag {
    pub host: HostId,
    pub paths: Vec<PathBuf>,
}

impl TreePathDrag {
    pub fn local_sources(&self) -> Option<&[PathBuf]> {
        self.paths_on(HostId::LOCAL)
    }
    pub fn paths_on(&self, host: HostId) -> Option<&[PathBuf]> {
        (self.host == host).then_some(self.paths.as_slice())
    }
}

pub fn same_local_tree(destination: HostId, src: &Path, dir: &Path) -> bool {
    destination.is_local() && (src.parent() == Some(dir) || dir == src || dir.starts_with(src))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tree_drag_source_identity_survives_same_named_paths() {
        let remote = HostId::from_connection_key("remote-drag");
        let local = TreePathDrag {
            host: HostId::LOCAL,
            paths: vec!["/same/name".into()],
        };
        let foreign = TreePathDrag {
            host: remote,
            paths: local.paths.clone(),
        };
        assert_eq!(local.local_sources(), Some(local.paths.as_slice()));
        assert!(foreign.local_sources().is_none());
        assert!(foreign.paths_on(HostId::LOCAL).is_none());
        assert_eq!(foreign.paths_on(remote), Some(foreign.paths.as_slice()));
        assert!(local.paths_on(remote).is_none());
    }
    #[test]
    fn drop_self_comparison_is_machine_scoped() {
        let remote = HostId::from_connection_key("copy-target");
        let src = Path::new("/same/pkg");
        for dir in [Path::new("/same"), src, Path::new("/same/pkg/sub")] {
            assert!(same_local_tree(HostId::LOCAL, src, dir));
            assert!(!same_local_tree(remote, src, dir));
        }
        assert!(!same_local_tree(HostId::LOCAL, src, Path::new("/other")));
    }
}
