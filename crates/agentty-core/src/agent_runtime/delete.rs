use std::io;
use std::path::PathBuf;

use crate::host::Host;

use super::navigator::DeletePlan;

const MAX_CODEX_INDEX_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionDeleteSource {
    File(PathBuf),
    Directory(PathBuf),
    CodexIndexEntry { path: PathBuf, session_id: String },
}

pub fn plan_session_delete_source(
    plan: &DeletePlan,
    roots: &super::AgentStoreRoots,
) -> io::Result<SessionDeleteSource> {
    let source = plan.source_path.as_deref().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "session has no backing source")
    })?;
    let path = PathBuf::from(source);
    let provider = match &plan.identity {
        super::navigator::SessionIdentity::Provider(key) => provider_id(&key.provider)?,
        super::navigator::SessionIdentity::Durable(_) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "durable live-only sessions have no provider source to delete",
            ));
        }
    };
    let descriptor = super::descriptor_for_id(provider);
    if !descriptor.accepts_source(roots, &path) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "refusing to delete noncanonical {} session source: {}",
                provider.slug(),
                path.display()
            ),
        ));
    }
    if provider == super::ProviderId::Codex
        && path.file_name().and_then(|name| name.to_str()) == Some("session_index.jsonl")
    {
        let session_id = plan.session_id.clone().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "Codex index row has no session id",
            )
        })?;
        Ok(SessionDeleteSource::CodexIndexEntry { path, session_id })
    } else if provider == super::ProviderId::Grok {
        let session_dir = path.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "Grok summary.json has no session directory",
            )
        })?;
        Ok(SessionDeleteSource::Directory(session_dir.to_path_buf()))
    } else {
        Ok(SessionDeleteSource::File(path))
    }
}

fn provider_id(slug: &str) -> io::Result<super::ProviderId> {
    super::PERSISTED_PROVIDER_DESCRIPTORS
        .iter()
        .find(|descriptor| descriptor.id.slug() == slug)
        .map(|descriptor| descriptor.id)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("unknown persisted session provider: {slug}"),
            )
        })
}

/// Close-and-Delete for live rows that do not yet have a provider backing
/// file returns `Ok(None)` so the UI can tombstone without a false failure.
pub fn plan_close_and_delete_source(
    plan: &DeletePlan,
    roots: &super::AgentStoreRoots,
) -> io::Result<Option<SessionDeleteSource>> {
    match &plan.identity {
        super::navigator::SessionIdentity::Durable(_) => Ok(None),
        super::navigator::SessionIdentity::Provider(_) if plan.source_path.is_none() => Ok(None),
        super::navigator::SessionIdentity::Provider(_) => {
            plan_session_delete_source(plan, roots).map(Some)
        }
    }
}

/// Clears Environment user state for a deleted identity without removing a
/// provider source file (used when Close and Delete has no backing path yet).
pub fn apply_session_user_state_delete(
    host: &dyn Host,
    alias_path: &std::path::Path,
    aliases: &super::SessionUserStateStore,
    environment: &crate::core::environment::EnvironmentId,
    identity: &super::SessionIdentity,
) -> io::Result<super::SessionUserStateStore> {
    let mut candidate = aliases.clone();
    candidate.delete(environment, identity);
    candidate.save(host, alias_path)?;
    Ok(candidate)
}

pub fn apply_session_delete_source(
    host: &dyn Host,
    source: &SessionDeleteSource,
) -> io::Result<()> {
    match source {
        SessionDeleteSource::File(path) => match host.remove(path, false) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        },
        SessionDeleteSource::Directory(path) => match host.remove(path, true) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        },
        SessionDeleteSource::CodexIndexEntry { path, session_id } => {
            let bytes = host.read_file(path, MAX_CODEX_INDEX_BYTES)?;
            let text = std::str::from_utf8(&bytes).map_err(|error| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("Codex index is not UTF-8: {error}"),
                )
            })?;
            let mut kept = Vec::new();
            let mut removed = false;
            for (index, line) in text.lines().enumerate() {
                let value: serde_json::Value = serde_json::from_str(line).map_err(|error| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("invalid Codex index JSONL at line {}: {error}", index + 1),
                    )
                })?;
                let matches = super::parse::codex_index_metadata(&value)
                    .and_then(|metadata| metadata.session_id)
                    .as_deref()
                    == Some(session_id.as_str());
                if matches {
                    removed = true;
                } else {
                    kept.push(line);
                }
            }
            if !removed {
                return Ok(());
            }
            let mut output = kept.join("\n").into_bytes();
            if !output.is_empty() {
                output.push(b'\n');
            }
            host.write_file(path, &output).map(|_| ())
        }
    }
}

pub fn apply_session_delete_transaction(
    host: &dyn Host,
    source: &SessionDeleteSource,
    alias_path: &std::path::Path,
    aliases: &super::SessionUserStateStore,
    environment: &crate::core::environment::EnvironmentId,
    identity: &super::SessionIdentity,
) -> io::Result<super::SessionUserStateStore> {
    let mut candidate = aliases.clone();
    candidate.delete(environment, identity);
    candidate.save(host, alias_path)?;
    if let Err(source_error) = apply_session_delete_source(host, source) {
        if let Err(rollback_error) = aliases.save(host, alias_path) {
            return Err(io::Error::new(
                source_error.kind(),
                format!("{source_error}; alias rollback also failed: {rollback_error}"),
            ));
        }
        return Err(source_error);
    }
    Ok(candidate)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_runtime::{AgentSessionKey, AgentStoreRoots};
    use crate::core::cli_agent::CLIAgent;
    use crate::host::local::LocalHost;
    use std::path::Path;

    fn plan(provider: &str, session_id: &str, source: &Path) -> DeletePlan {
        let mut navigator = crate::agent_runtime::SessionNavigator::default();
        navigator.refresh(
            &[crate::agent_runtime::AgentSessionRecord {
                key: AgentSessionKey {
                    provider: provider.into(),
                    session_id: session_id.into(),
                },
                agent: match provider {
                    "claude" => CLIAgent::Claude,
                    "omp" => CLIAgent::Omp,
                    _ => CLIAgent::Codex,
                },
                title: Some("Delete me".into()),
                title_candidates: Default::default(),
                cwd: None,
                updated_at_unix_ms: None,
                launch_argv: Vec::new(),
                source_path: Some(source.to_string_lossy().into_owned()),
                created_at_unix_ms: None,
            }],
            &[],
        );
        navigator
            .plan_delete(&navigator.rows()[0].row_id)
            .expect("historical row has a delete plan")
    }

    #[test]
    fn direct_session_delete_source_removes_only_backing_file() {
        let temp = tempfile::tempdir().unwrap();
        let roots = AgentStoreRoots::for_home(temp.path().to_path_buf());
        let source = roots.claude_projects().join("repo/target.jsonl");
        let neighbor = roots.claude_projects().join("repo/neighbor.jsonl");
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(&source, "target\n").unwrap();
        std::fs::write(&neighbor, "neighbor\n").unwrap();

        let target = plan_session_delete_source(&plan("claude", "s1", &source), &roots).unwrap();
        apply_session_delete_source(&*LocalHost::new(), &target).unwrap();

        assert!(!source.exists());
        assert!(neighbor.exists());
    }

    #[test]
    fn codex_index_delete_source_rewrites_only_matching_entry() {
        let temp = tempfile::tempdir().unwrap();
        let roots = AgentStoreRoots::for_home(temp.path().to_path_buf());
        let index = roots.codex_index();
        std::fs::create_dir_all(index.parent().unwrap()).unwrap();
        std::fs::write(
            &index,
            concat!(
                "{\"id\":\"019fa76a-6276-7b03-b302-c640686b2033\",\"title\":\"remove\"}\n",
                "{\"id\":\"019fa76a-6276-7b03-b302-c640686b2044\",\"title\":\"keep\"}\n"
            ),
        )
        .unwrap();
        let target = plan_session_delete_source(
            &plan("codex", "019fa76a-6276-7b03-b302-c640686b2033", &index),
            &roots,
        )
        .unwrap();
        apply_session_delete_source(&*LocalHost::new(), &target).unwrap();

        let written = std::fs::read_to_string(index).unwrap();
        assert!(!written.contains("019fa76a-6276-7b03-b302-c640686b2033"));
        assert!(written.contains("019fa76a-6276-7b03-b302-c640686b2044"));
    }

    #[test]
    fn malformed_codex_index_fails_without_overwriting_source() {
        let temp = tempfile::tempdir().unwrap();
        let roots = AgentStoreRoots::for_home(temp.path().to_path_buf());
        let index = roots.codex_index();
        std::fs::create_dir_all(index.parent().unwrap()).unwrap();
        let original = b"{not-json}\n";
        std::fs::write(&index, original).unwrap();

        let target = plan_session_delete_source(
            &plan("codex", "019fa76a-6276-7b03-b302-c640686b2033", &index),
            &roots,
        )
        .unwrap();
        assert!(apply_session_delete_source(&*LocalHost::new(), &target).is_err());
        assert_eq!(std::fs::read(index).unwrap(), original);
    }

    #[test]
    fn provider_delete_failure_restores_alias_store() {
        let temp = tempfile::tempdir().unwrap();
        let roots = AgentStoreRoots::for_home(temp.path().to_path_buf());
        let source = roots.claude_projects().join("non-empty-source.jsonl");
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("keeps-directory-non-empty"), b"fixture").unwrap();
        let alias_path = temp.path().join("session-aliases.json");
        let host = LocalHost::new();
        let delete_plan = plan("claude", "s1", &source);
        let target = plan_session_delete_source(&delete_plan, &roots).unwrap();
        let environment = crate::core::environment::EnvironmentId::local();
        let mut aliases = crate::agent_runtime::SessionUserStateStore::default();
        aliases
            .set(
                environment.clone(),
                delete_plan.identity.clone(),
                Some("Keep me".into()),
            )
            .unwrap();
        aliases.save(&*host, &alias_path).unwrap();

        assert!(
            apply_session_delete_transaction(
                &*host,
                &target,
                &alias_path,
                &aliases,
                &environment,
                &delete_plan.identity,
            )
            .is_err()
        );
        let loaded =
            crate::agent_runtime::SessionUserStateStore::load(&*host, &alias_path).unwrap();
        assert_eq!(
            loaded.alias(&environment, &delete_plan.identity),
            Some("Keep me")
        );
    }

    #[test]
    fn plan_close_and_delete_source_allows_missing_provider_file() {
        let roots = AgentStoreRoots::for_home(tempfile::tempdir().unwrap().into_path());
        let mut navigator = crate::agent_runtime::SessionNavigator::default();
        navigator.refresh(
            &[],
            &[crate::agent_runtime::LiveSession {
                identity: crate::agent_runtime::SessionIdentity::Durable("container-1".into()),
                agent: CLIAgent::Codex,
                session_id: None,
                title: None,
                title_candidates: Default::default(),
                cwd: None,
                launch_argv: Vec::new(),
                carrier: crate::agent_runtime::LiveCarrier {
                    container_id: "container-1".into(),
                    tab_id: Some("t".into()),
                    pane_id: Some(1),
                },
                execution: None,
            }],
        );
        let plan = navigator
            .plan_close_and_delete(&navigator.rows()[0].row_id)
            .expect("live row plans close-and-delete");
        assert!(
            plan_close_and_delete_source(&plan, &roots)
                .unwrap()
                .is_none()
        );

        navigator.refresh(
            &[crate::agent_runtime::AgentSessionRecord {
                key: AgentSessionKey {
                    provider: "codex".into(),
                    session_id: "s-new".into(),
                },
                agent: CLIAgent::Codex,
                title: None,
                title_candidates: Default::default(),
                cwd: None,
                updated_at_unix_ms: None,
                launch_argv: Vec::new(),
                source_path: None,
                created_at_unix_ms: None,
            }],
            &[crate::agent_runtime::LiveSession {
                identity: crate::agent_runtime::SessionIdentity::Provider(AgentSessionKey {
                    provider: "codex".into(),
                    session_id: "s-new".into(),
                }),
                agent: CLIAgent::Codex,
                session_id: Some("s-new".into()),
                title: None,
                title_candidates: Default::default(),
                cwd: None,
                launch_argv: Vec::new(),
                carrier: crate::agent_runtime::LiveCarrier {
                    container_id: "container-2".into(),
                    tab_id: Some("t2".into()),
                    pane_id: Some(2),
                },
                execution: None,
            }],
        );
        let live_id = navigator
            .rows()
            .iter()
            .find(|row| row.lifecycle == crate::agent_runtime::RowLifecycle::Live)
            .map(|row| row.row_id.clone())
            .expect("live row");
        let plan = navigator.plan_close_and_delete(&live_id).unwrap();
        assert!(plan.source_path.is_none());
        assert!(
            plan_close_and_delete_source(&plan, &roots)
                .unwrap()
                .is_none()
        );
    }
}
