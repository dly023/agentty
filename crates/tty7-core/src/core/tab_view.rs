//! What a tab looks like to someone who is not the window showing it.
//!
//! A window renders its own tabs from live terminals: OSC titles, agent
//! chatter, unread counts. Everyone else — the switcher listing a workspace
//! it does not own, `tty7 tab ls` on the other side of a socket — has only
//! the machine tree. This is the reading of that tree, kept in one place so
//! the CLI and the GUI name a tab the same way.

use crate::core::cli_agent::{AgentStatus, CLIAgent};
use crate::core::machine::{PaneRecord, TabId, Workspace};

/// Deliberately not serialisable: it is a reading of the machine tree, and
/// both sides that want one have the tree already. Putting it on the wire
/// would be sending a conclusion where the evidence has already gone.
#[derive(Debug, Clone, PartialEq)]
pub struct TabView {
    pub id: TabId,
    pub name: Option<String>,
    /// The foreground process of the tab's leading pane — "zsh", "vim".
    pub title: String,
    /// The title the tab's terminal reported over OSC 0/2, which is the name the
    /// window that owns it puts on its tab. See
    /// [`PaneRecord::osc_title`](crate::core::machine::PaneRecord::osc_title).
    pub osc_title: Option<String>,
    pub provider_title: Option<String>,
    pub cwd: Option<String>,
    pub agent: Option<CLIAgent>,
    pub status: Option<AgentStatus>,
    pub live: bool,
    pub panes: usize,
    pub recovery: Option<PaneRecovery>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PaneRecovery {
    pub pane: u64,
    pub binding: crate::core::machine::AgentRecoveryBinding,
    pub pane_live: bool,
}

/// Where a tab's displayed name comes from, best evidence first. Callers
/// render it themselves: a path is abbreviated one way in a 20-column tab
/// strip and another way in a terminal table, and only the GUI has a
/// translated string for a tab with nothing to say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabLabel<'a> {
    /// Someone named this tab, so nothing else gets a say.
    Named(&'a str),
    Provider(&'a str),
    /// The terminal's own title. Second only to a given name because it is what
    /// the window owning the tab is showing: a shell writes where it is, an
    /// agent writes what it is doing, and either way disagreeing with the tab
    /// strip would be worse than any ranking of our own.
    ///
    /// It may well be a path (`user@host:~/dir` is what the shell integration
    /// sets), so a caller that abbreviates [`Cwd`](Self::Cwd) has to abbreviate
    /// this too.
    Osc(&'a str),
    /// No name and no title, but an agent is running in it — which is what
    /// anyone scanning a list of tabs is looking for.
    Agent(CLIAgent),
    /// The working directory of the tab's leading pane.
    Cwd(&'a str),
    /// The foreground process name. Thin, but it beats nothing.
    Process(&'a str),
    /// A tab holding a pane the tree knows nothing about.
    Unknown,
}

/// Cuts the `user@host:` head that a shell integration writes into its title,
/// leaving the path (or command) it actually names. A title with no such head —
/// an agent's, which is prose — comes back untouched, and so does a bare
/// `host:`: that is a drive letter on Windows.
///
/// What stops the head being a head is a *port* after it: a tail of nothing
/// but digits makes the whole string an address rather than a titled
/// directory. `deploy@10.0.0.5:2222` is what a freshly dialled SSH pane calls
/// itself, and cutting it left the tab labelled with nothing but a port
/// number (#438).
///
/// Only a port. Anything else after the colon is a path and is kept, because
/// the paths that arrive here are not all `/…` or `~/…`: tty7's own PowerShell
/// integration writes `ann@BOX:C:/src` for a cwd off the home drive, and
/// Debian's stock bash title is `\u@\h: \w` — a space, which belongs to the
/// head rather than to the path.
///
/// Here rather than in either renderer because both of them need it and they
/// have to agree: the GUI abbreviates the path that comes out, the CLI takes its
/// last segment, and neither can start by guessing where the path begins.
pub fn strip_host_prefix(raw: &str) -> &str {
    let Some((head, tail)) = raw.split_once(':') else {
        return raw;
    };
    if !head.contains('@') {
        return raw;
    }
    let is_port = !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit());
    match is_port {
        true => raw,
        false => tail.trim_start(),
    }
}

impl TabView {
    pub fn label(&self) -> TabLabel<'_> {
        if let Some(name) = self
            .name
            .as_deref()
            .map(str::trim)
            .filter(|n| !n.is_empty())
        {
            return TabLabel::Named(name);
        }
        if let Some(title) = self
            .provider_title
            .as_deref()
            .filter(|title| !title.trim().is_empty())
        {
            return TabLabel::Provider(title);
        }
        if let Some(title) = self
            .osc_title
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
        {
            return TabLabel::Osc(title);
        }
        if let Some(agent) = self.agent {
            return TabLabel::Agent(agent);
        }
        if let Some(cwd) = self.cwd.as_deref().map(str::trim).filter(|c| !c.is_empty()) {
            return TabLabel::Cwd(cwd);
        }
        match self.title.trim() {
            "" => TabLabel::Unknown,
            title => TabLabel::Process(title),
        }
    }
}

pub fn tab_views_of(ws: &Workspace, panes: &[PaneRecord]) -> Vec<TabView> {
    ws.tabs
        .iter()
        .map(|tab| {
            let ids = tab.root.pane_ids();
            let records: Vec<&PaneRecord> = ids
                .iter()
                .filter_map(|id| panes.iter().find(|p| p.id == *id))
                .collect();
            // The first pane stands in for the tab, the same way the strip shows
            // its focused leaf — but any pane running an agent wins, since that
            // is what someone scanning the list is looking for.
            let head = records.first();
            let facts = records.iter().find_map(|p| p.agent.as_ref());
            // The title follows the agent for the same reason the facts do: an
            // agent's pane titles itself with what it is working on, while a
            // plain shell's says where it is — which `cwd` carries anyway. A
            // split with a shell in front would otherwise name the tab after a
            // directory and bury the agent.
            let titled = records.iter().find(|p| p.agent.is_some()).or(head);
            TabView {
                id: tab.id,
                name: tab.name.clone(),
                title: head.map(|p| p.title.clone()).unwrap_or_default(),
                osc_title: titled.and_then(|p| p.osc_title.clone()),
                provider_title: titled.and_then(|p| p.provider_title_text().map(str::to_string)),
                cwd: head.and_then(|p| p.cwd.clone()),
                agent: facts.map(|f| f.agent),
                status: facts.and_then(|f| f.status),
                live: records.iter().any(|p| p.live),
                panes: ids.len(),
                recovery: if records.iter().any(|p| p.live && p.agent.is_some()) {
                    None
                } else {
                    records.iter().find_map(|p| {
                        p.recovery_binding.clone().map(|binding| PaneRecovery {
                            pane: p.id,
                            binding,
                            pane_live: p.live,
                        })
                    })
                },
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::machine::{AgentFacts, Tab};

    #[test]
    fn live_title_keeps_alias_and_rejects_replaced_identity() {
        use crate::core::machine::ProviderTitle;
        let mut pane = PaneRecord::new(1);
        pane.observe_agent_identity(
            Some(AgentFacts {
                agent: CLIAgent::Codex,
                session_id: Some("session-a".into()),
                launch_argv: None,
                status: None,
            }),
            Some("/work"),
        );
        pane.provider_title = Some(ProviderTitle {
            agent: CLIAgent::Codex,
            session_id: "session-a".into(),
            title: "官方名称".into(),
        });
        let mut ws = Workspace::default();
        ws.tabs.push(Tab::leaf(1));
        assert_eq!(
            tab_views_of(&ws, &[pane.clone()])[0].label(),
            TabLabel::Provider("官方名称")
        );
        ws.tabs[0].name = Some("手动命名".into());
        assert_eq!(
            tab_views_of(&ws, &[pane.clone()])[0].label(),
            TabLabel::Named("手动命名")
        );
        pane.observe_agent_identity(
            Some(AgentFacts {
                agent: CLIAgent::Codex,
                session_id: Some("session-b".into()),
                launch_argv: None,
                status: None,
            }),
            Some("/work"),
        );
        assert!(pane.provider_title.is_none());
    }

    fn view() -> TabView {
        TabView {
            id: TabId::new(),
            name: None,
            title: String::new(),
            osc_title: None,
            provider_title: None,
            cwd: None,
            agent: None,
            status: None,
            live: true,
            panes: 1,
            recovery: None,
        }
    }

    #[test]
    fn tab_recovery_uses_bound_pane_liveness_and_defers_to_other_live_agent() {
        use crate::core::machine::{AgentRecoveryBinding, Axis, PaneNode};
        let mut tab = Tab::leaf(1);
        tab.root = PaneNode::Split {
            axis: Axis::Horizontal,
            ratio: 0.5,
            a: Box::new(PaneNode::Leaf { pane: 1 }),
            b: Box::new(PaneNode::Leaf { pane: 2 }),
        };
        let ws = Workspace {
            tabs: vec![tab],
            ..Workspace::default()
        };
        let mut panes = vec![PaneRecord::new(1), PaneRecord::new(2)];
        panes[0].live = true;
        panes[1].recovery_binding = Some(AgentRecoveryBinding {
            agent: CLIAgent::Codex,
            session_id: "bound-second-pane".into(),
            cwd: "/original".into(),
            launch_argv: None,
        });
        let projected = tab_views_of(&ws, &panes).remove(0);
        assert!(projected.live, "the first shell is still alive");
        let recovery = projected.recovery.unwrap();
        assert_eq!(recovery.pane, 2);
        assert!(
            !recovery.pane_live,
            "tab liveness cannot revive the bound pane"
        );
        panes[0].agent = Some(AgentFacts {
            agent: CLIAgent::Claude,
            session_id: None,
            launch_argv: None,
            status: Some(AgentStatus::Working),
        });
        assert!(tab_views_of(&ws, &panes)[0].recovery.is_none());
        panes[0].live = false;
        assert!(
            tab_views_of(&ws, &panes)[0].recovery.is_some(),
            "stopped agent facts cannot hide the recovery candidate"
        );
    }

    #[test]
    fn tab_recovery_distinguishes_exited_agent_from_stopped_pane() {
        use crate::core::machine::{AgentFacts, AgentRecoveryBinding, Tab};
        let mut ws = Workspace::default();
        ws.tabs.push(Tab::leaf(1));
        let mut record = PaneRecord::new(1);
        record.live = true;
        record.recovery_binding = Some(AgentRecoveryBinding {
            agent: CLIAgent::Codex,
            session_id: "saved-session".into(),
            cwd: "/original".into(),
            launch_argv: None,
        });
        let projected = tab_views_of(&ws, &[record.clone()]).remove(0);
        let recovery = projected.recovery.expect("exited agent remains visible");
        assert_eq!(recovery.pane, 1);
        assert!(recovery.pane_live);
        assert_eq!(recovery.binding.session_id, "saved-session");
        assert!(projected.agent.is_none());
        record.live = false;
        assert!(
            !tab_views_of(&ws, &[record.clone()])[0]
                .recovery
                .as_ref()
                .unwrap()
                .pane_live
        );
        record.live = true;
        record.agent = Some(AgentFacts {
            agent: CLIAgent::Claude,
            session_id: None,
            launch_argv: None,
            status: None,
        });
        assert!(
            tab_views_of(&ws, &[record])[0].recovery.is_none(),
            "a live agent outranks old binding"
        );
    }

    #[test]
    fn a_label_prefers_the_name_then_the_title_then_the_agent_then_the_place() {
        let named = TabView {
            name: Some("  deploy  ".into()),
            osc_title: Some("✳ fixing the switcher".into()),
            agent: Some(CLIAgent::Claude),
            cwd: Some("/work".into()),
            ..view()
        };
        assert_eq!(named.label(), TabLabel::Named("deploy"));

        // The window owning this tab shows the title its agent set, so the
        // switcher listing the same tab has to show it too — naming it after
        // the agent is what made every tab of a workspace read "Claude Code".
        let titled = TabView {
            osc_title: Some("  ✳ fixing the switcher  ".into()),
            agent: Some(CLIAgent::Claude),
            cwd: Some("/work".into()),
            ..view()
        };
        assert_eq!(titled.label(), TabLabel::Osc("✳ fixing the switcher"));

        let blank_title = TabView {
            osc_title: Some("   ".into()),
            agent: Some(CLIAgent::Claude),
            ..view()
        };
        assert_eq!(blank_title.label(), TabLabel::Agent(CLIAgent::Claude));

        let working = TabView {
            agent: Some(CLIAgent::Claude),
            cwd: Some("/work".into()),
            ..view()
        };
        assert_eq!(working.label(), TabLabel::Agent(CLIAgent::Claude));

        let plain = TabView {
            cwd: Some("/work".into()),
            title: "zsh".into(),
            ..view()
        };
        assert_eq!(plain.label(), TabLabel::Cwd("/work"));
    }

    #[test]
    fn a_blank_name_is_no_name_and_a_bare_shell_falls_back_to_its_process() {
        let blank = TabView {
            name: Some("   ".into()),
            title: "zsh".into(),
            ..view()
        };
        assert_eq!(blank.label(), TabLabel::Process("zsh"));
        assert_eq!(view().label(), TabLabel::Unknown);
    }

    #[test]
    fn a_tab_is_read_through_its_leading_pane_but_any_agent_in_it_wins() {
        let mut ws = Workspace::default();
        let mut tab = Tab::leaf(1);
        tab.root = crate::core::machine::PaneNode::Split {
            axis: crate::core::machine::Axis::Horizontal,
            ratio: 0.5,
            a: Box::new(crate::core::machine::PaneNode::Leaf { pane: 1 }),
            b: Box::new(crate::core::machine::PaneNode::Leaf { pane: 2 }),
        };
        ws.tabs.push(tab);

        let panes = vec![
            PaneRecord {
                cwd: Some("/work".into()),
                title: "zsh".into(),
                osc_title: Some("user@host:~/work".into()),
                live: true,
                ..PaneRecord::new(1)
            },
            PaneRecord {
                osc_title: Some("✳ fixing the switcher".into()),
                agent: Some(AgentFacts {
                    agent: CLIAgent::Claude,
                    session_id: None,
                    launch_argv: None,
                    status: None,
                }),
                ..PaneRecord::new(2)
            },
        ];

        let views = tab_views_of(&ws, &panes);
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].cwd.as_deref(), Some("/work"));
        assert_eq!(views[0].agent, Some(CLIAgent::Claude));
        assert_eq!(views[0].panes, 2);
        assert!(views[0].live, "one live pane makes the tab live");
        assert_eq!(
            views[0].osc_title.as_deref(),
            Some("✳ fixing the switcher"),
            "the agent's pane names the tab, not the shell in front of it"
        );
    }

    #[test]
    fn a_host_prefix_is_only_cut_when_a_path_follows_it() {
        assert_eq!(strip_host_prefix("user@host:~/work"), "~/work");
        assert_eq!(strip_host_prefix("user@host:/srv/app"), "/srv/app");
        assert_eq!(
            strip_host_prefix("user@host: ~/work"),
            "~/work",
            "Debian's stock bash title puts a space after the colon"
        );
        assert_eq!(
            strip_host_prefix("ann@BOX:C:/src"),
            "C:/src",
            "tty7's own pwsh title names a drive when the cwd is off the home drive"
        );
        assert_eq!(strip_host_prefix("user@host:   "), "");
        assert_eq!(
            strip_host_prefix("deploy@10.0.0.5:2222"),
            "deploy@10.0.0.5:2222",
            "an address is the name, not a head to cut off it"
        );
        assert_eq!(
            strip_host_prefix("user@host:"),
            "",
            "a shell that has not placed itself yet leaves nothing to show"
        );
        assert_eq!(strip_host_prefix("C:/src"), "C:/src");
        assert_eq!(strip_host_prefix("vim — main.rs"), "vim — main.rs");
    }
}
