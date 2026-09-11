use std::time::Duration;

use gpui::{
    Animation, AnimationExt as _, Context, EventEmitter, FocusHandle, Focusable, SharedString,
    Window, div, prelude::*, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _, h_flex, v_flex,
};

use crate::daemon::protocol::ShellSpec;
use crate::terminal::PaneWorkspace;
use crate::ui::i18n::{L10nKey, t_fmt};

#[derive(Clone)]
pub struct PendingSpawn {
    pub workspace: Option<PaneWorkspace>,
    pub working_directory: Option<std::path::PathBuf>,
    pub restore_pane: Option<u64>,
    pub restore_policy: crate::terminal::RestorePolicy,
    pub shell: Option<ShellSpec>,
    pub agent: Option<crate::core::cli_agent::CLIAgent>,
    pub agent_session_id: Option<String>,
    pub agent_launch_argv: Option<Vec<String>>,
    pub explicit_resume: Option<crate::core::cli_agent::ResumeInvocation>,
    pub owner: Option<crate::core::session::WorkspaceId>,
    pub font_size: f32,
}

impl PendingSpawn {
    pub fn take_resume_command(&mut self, restored: bool, automatic: bool) -> Option<String> {
        let explicit = self.explicit_resume.take();
        let attach_only = self.restore_policy.attach_only();
        if explicit.is_some() || attach_only {
            self.agent = None;
            self.agent_session_id = None;
            self.agent_launch_argv = None;
        }
        if restored || attach_only {
            return None;
        }
        if let Some(invocation) = explicit {
            return Some(invocation.command_line());
        }
        if !automatic {
            return None;
        }
        self.agent?.resume_command(
            self.agent_session_id.as_deref()?,
            self.agent_launch_argv.as_deref(),
        )
    }
}

pub enum PendingState {
    Stopped,
    Connecting,
    Failed(SharedString),
}

pub struct RetryRequested;

#[cfg(test)]
mod resume_tests {
    use super::*;
    use crate::core::cli_agent::CLIAgent;

    #[test]
    fn checked_restore_pending_preserves_guards_and_never_resumes() {
        let guard = crate::daemon::protocol::CheckedPaneAttachment {
            identity: crate::daemon::protocol::PaneAttachExpectation::Shell,
            proof: tty7_core::daemon::control::AttachmentProof {
                workspace: crate::core::session::WorkspaceId::new(),
                nonce: uuid::Uuid::new_v4(),
            },
            tab: tty7_core::core::machine::TabId::new(),
        };
        let mut pending = PendingSpawn {
            workspace: None,
            working_directory: None,
            restore_pane: Some(7),
            restore_policy: crate::terminal::RestorePolicy::Checked(std::sync::Arc::new(
                [(7, guard)].into(),
            )),
            shell: None,
            agent: Some(CLIAgent::Codex),
            agent_session_id: Some("01900000-0000-7000-8000-000000000001".into()),
            agent_launch_argv: None,
            explicit_resume: Some(
                CLIAgent::Codex
                    .resume_invocation("01900000-0000-7000-8000-000000000001")
                    .unwrap(),
            ),
            owner: None,
            font_size: 14.,
        };
        let retry = pending.clone();
        assert_eq!(retry.restore_policy, pending.restore_policy);
        assert_eq!(pending.take_resume_command(false, true), None);
        assert_eq!(pending.take_resume_command(false, true), None);
    }

    #[test]
    fn attach_only_pending_never_delivers_resume_even_if_completion_is_wrong() {
        let id = "01900000-0000-7000-8000-000000000001";
        let pending = PendingSpawn {
            workspace: None,
            working_directory: None,
            restore_pane: Some(7),
            restore_policy: crate::terminal::RestorePolicy::AttachOnly,
            shell: None,
            agent: Some(CLIAgent::Codex),
            agent_session_id: Some(id.into()),
            agent_launch_argv: None,
            explicit_resume: Some(CLIAgent::Codex.resume_invocation(id).unwrap()),
            owner: None,
            font_size: 14.,
        };
        for restored in [false, true] {
            for automatic in [false, true] {
                let mut retry = pending.clone();
                assert_eq!(
                    retry.restore_policy,
                    crate::terminal::RestorePolicy::AttachOnly
                );
                assert_eq!(retry.take_resume_command(restored, automatic), None);
                assert_eq!(retry.take_resume_command(false, true), None);
            }
        }
    }

    #[test]
    fn explicit_resume_cannot_reenter_automatic_fallback() {
        let id = "01900000-0000-7000-8000-000000000001";
        let invocation = CLIAgent::Codex.resume_invocation(id).unwrap();
        for restored in [false, true] {
            let mut pending = PendingSpawn {
                restore_policy: crate::terminal::RestorePolicy::ReplaceMissing,
                workspace: None,
                working_directory: None,
                restore_pane: None,
                shell: None,
                agent: Some(CLIAgent::Codex),
                agent_session_id: Some(id.into()),
                agent_launch_argv: Some(vec!["codex".into()]),
                explicit_resume: Some(invocation.clone()),
                owner: None,
                font_size: 14.,
            };
            assert_eq!(
                pending.take_resume_command(restored, true),
                (!restored).then(|| invocation.command_line())
            );
            assert_eq!(
                pending.take_resume_command(false, true),
                None,
                "consumed explicit intent cannot run again as automatic restore"
            );
            assert!(pending.agent.is_none());
            assert!(pending.agent_session_id.is_none());
            assert!(pending.agent_launch_argv.is_none());
        }
    }

    #[test]
    fn explicit_pending_resume_is_one_shot_and_not_auto_resume() {
        let invocation = CLIAgent::Codex
            .resume_invocation("01900000-0000-7000-8000-000000000001")
            .unwrap();
        let spawn = || PendingSpawn {
            restore_policy: crate::terminal::RestorePolicy::ReplaceMissing,
            workspace: None,
            working_directory: None,
            restore_pane: None,
            shell: None,
            agent: None,
            agent_session_id: None,
            agent_launch_argv: None,
            explicit_resume: Some(invocation.clone()),
            owner: None,
            font_size: 14.,
        };
        let mut pending = spawn();
        assert_eq!(
            pending.take_resume_command(false, false),
            Some(invocation.command_line()),
            "explicit Resume must work with auto-restore disabled"
        );
        assert_eq!(
            pending.take_resume_command(false, false),
            None,
            "the explicit command is consumed exactly once"
        );
        let mut rejoined = spawn();
        assert_eq!(
            rejoined.take_resume_command(true, true),
            None,
            "an attached live pane must never receive a launch command"
        );
        assert!(rejoined.explicit_resume.is_none());
    }
}

pub struct PendingPane {
    pub focus_handle: FocusHandle,
    pub machine: SharedString,
    pub state: PendingState,
    pub spawn: PendingSpawn,
    // Presentation only: recovery identity always comes from the target mirror.
    // Keep the old screen until canonical landing replaces this controller.
    pub(crate) retained_view: Option<gpui::Entity<crate::terminal::view::TerminalView>>,
    pub(crate) resume: Option<super::session_recovery::ResumeUi>,
    pub(crate) resume_unavailable: bool,
    pub(crate) output_unavailable: bool,
    resume_generation: u64,
}

impl EventEmitter<RetryRequested> for PendingPane {}
impl EventEmitter<super::session_recovery::ResumeRequested> for PendingPane {}

impl PendingPane {
    pub fn new(
        machine: impl Into<SharedString>,
        spawn: PendingSpawn,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            machine: machine.into(),
            state: PendingState::Connecting,
            spawn,
            retained_view: None,
            resume: None,
            resume_unavailable: false,
            output_unavailable: false,
            resume_generation: 0,
        }
    }

    pub fn fail(&mut self, reason: impl Into<SharedString>, cx: &mut Context<Self>) {
        if matches!(self.state, PendingState::Stopped) {
            return;
        }
        self.state = PendingState::Failed(reason.into());
        cx.notify();
    }

    pub fn retrying(&mut self, cx: &mut Context<Self>) {
        if matches!(self.state, PendingState::Stopped) {
            return;
        }
        self.state = PendingState::Connecting;
        cx.notify();
    }

    pub(super) fn begin_resume(
        &mut self,
        target: super::session_recovery::ResumeTarget,
        refresh: bool,
        cx: &mut Context<Self>,
    ) {
        self.begin_recovery(
            target,
            if refresh {
                super::session_recovery::RecoveryAction::Refresh
            } else {
                super::session_recovery::RecoveryAction::Resume
            },
            cx,
        );
    }

    fn begin_recovery(
        &mut self,
        target: super::session_recovery::ResumeTarget,
        action: super::session_recovery::RecoveryAction,
        cx: &mut Context<Self>,
    ) {
        if !matches!(self.state, PendingState::Stopped)
            || self.resume.as_ref().is_some_and(|s| s.busy)
        {
            return;
        }
        self.resume_generation = self.resume_generation.wrapping_add(1);
        let request = super::session_recovery::ResumeRequested {
            target,
            generation: self.resume_generation,
            action,
        };
        self.output_unavailable = false;
        self.resume = Some(super::session_recovery::ResumeUi {
            request: request.clone(),
            busy: true,
        });
        cx.emit(request);
        cx.notify();
    }
}

impl Focusable for PendingPane {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for PendingPane {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (muted, dim) = (theme.muted_foreground, theme.muted_foreground.opacity(0.75));

        let body = match &self.state {
            PendingState::Stopped => {
                let target = self
                    .resume
                    .as_ref()
                    .map(|state| state.request.target.clone())
                    .or_else(|| {
                        super::session_recovery::candidate(
                            cx,
                            self.spawn.owner?,
                            self.spawn.restore_pane?,
                        )
                    });
                let busy = self.resume.as_ref().is_some_and(|s| s.busy);
                let output = self.resume.as_ref().is_some_and(|s| {
                    s.request.action == super::session_recovery::RecoveryAction::Output
                });
                let refresh = self.resume.is_some() && !output;
                v_flex()
                    .id("stopped-pane")
                    .debug_selector(|| "stopped-pane".into())
                    .items_center()
                    .gap(px(10.))
                    .max_w(px(420.))
                    .text_sm()
                    .text_color(theme.foreground)
                    .child(crate::ui::i18n::t(L10nKey::SessionTerminalStopped))
                    .when(busy, |el| {
                        el.child(crate::ui::i18n::t(if output {
                            L10nKey::SessionOutputLoading
                        } else {
                            L10nKey::HistoryResumePending
                        }))
                    })
                    .when(self.output_unavailable, |el| {
                        el.child(
                            div()
                                .text_center()
                                .text_color(muted)
                                .child(crate::ui::i18n::t(L10nKey::SessionOutputUnavailable)),
                        )
                    })
                    .when(refresh && !busy, |el| {
                        el.child(
                            div()
                                .text_center()
                                .text_color(muted)
                                .child(crate::ui::i18n::t(L10nKey::SessionResumeUnknown)),
                        )
                    })
                    .when(self.resume_unavailable && !refresh, |el| {
                        el.child(
                            div()
                                .text_center()
                                .text_color(muted)
                                .child(crate::ui::i18n::t(L10nKey::SessionResumeUnavailable)),
                        )
                    })
                    .when_some(target, |el, target| {
                        let output_target = target.clone();
                        let id = if refresh {
                            "stopped-pane-refresh"
                        } else {
                            "stopped-pane-resume"
                        };
                        el.child(
                            Button::new(id)
                                .debug_selector(move || id.into())
                                .label(crate::ui::i18n::t(if refresh {
                                    L10nKey::HistoryRefresh
                                } else {
                                    L10nKey::HistoryResume
                                }))
                                .ghost()
                                .small()
                                .disabled(busy)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.begin_resume(target.clone(), refresh, cx)
                                })),
                        )
                        .when(
                            !refresh && self.retained_view.is_none(),
                            |el| {
                                el.child(
                                    Button::new("stopped-pane-output")
                                        .debug_selector(|| "stopped-pane-output".into())
                                        .label(crate::ui::i18n::t(L10nKey::SessionViewOutput))
                                        .ghost()
                                        .small()
                                        .disabled(busy)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.begin_recovery(
                                                output_target.clone(),
                                                super::session_recovery::RecoveryAction::Output,
                                                cx,
                                            )
                                        })),
                                )
                            },
                        )
                    })
                    .into_any_element()
            }
            PendingState::Connecting => v_flex()
                .items_center()
                .gap(px(10.))
                .child(
                    Icon::new(IconName::LoaderCircle)
                        .size(px(18.))
                        .text_color(dim)
                        .with_animation(
                            "pending-pane-spin",
                            Animation::new(Duration::from_millis(900)).repeat(),
                            |icon, delta| {
                                icon.transform(gpui::Transformation::rotate(gpui::percentage(
                                    delta,
                                )))
                            },
                        ),
                )
                .child(div().text_sm().text_color(muted).child(t_fmt(
                    L10nKey::PendingConnecting,
                    &[("machine", &self.machine)],
                )))
                .into_any_element(),
            PendingState::Failed(reason) => v_flex()
                .items_center()
                .gap(px(10.))
                .max_w(px(420.))
                .child(div().text_sm().text_color(theme.foreground).child(t_fmt(
                    L10nKey::PendingUnreachable,
                    &[("machine", &self.machine)],
                )))
                .child(
                    div()
                        .text_xs()
                        .text_center()
                        .text_color(muted)
                        .child(reason.clone()),
                )
                .child(
                    Button::new("pending-pane-retry")
                        .debug_selector(|| "pending-pane-retry".into())
                        .label(crate::ui::i18n::t(crate::ui::i18n::L10nKey::TryAgain))
                        .ghost()
                        .small()
                        .on_click(cx.listener(|this, _, _window, cx| {
                            this.retrying(cx);
                            cx.emit(RetryRequested);
                        })),
                )
                .into_any_element(),
        };

        if let Some(view) = &self.retained_view {
            return div()
                .size_full()
                .relative()
                .track_focus(&self.focus_handle)
                .child(view.clone())
                .children(super::notice::anchor(vec![
                    super::notice::pill(theme.border, cx)
                        .child(body)
                        .into_any_element(),
                ]))
                .into_any_element();
        }
        h_flex()
            .track_focus(&self.focus_handle)
            .size_full()
            .items_center()
            .justify_center()
            .child(body)
            .into_any_element()
    }
}
