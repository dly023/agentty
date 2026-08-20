use std::collections::HashSet;

use gpui::{
    App, Axis, Context, Focusable as _, IntoElement, ParentElement as _, Styled as _, Window, div,
};
use gpui_component::ActiveTheme as _;

use crate::ui::app::{AgenttyApp, Tab};
use crate::ui::composer::PaneIdentity;
use crate::ui::pane::{Pane, PaneSlot, render_resizable_split};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DockLayout {
    PerColumn,
    TerminalsOnly,
}

pub(crate) fn dock_slots_for_tab(
    tab: &Tab,
    app: &AgenttyApp,
    window: &Window,
    cx: &App,
) -> Vec<PaneIdentity> {
    let mut slots = Vec::new();
    collect_dock_slots(
        &tab.pane,
        DockLayout::PerColumn,
        tab,
        app,
        window,
        cx,
        &mut slots,
    );
    slots.sort_by_key(|slot| slot.pane_id);
    slots.dedup();
    slots
}

fn collect_dock_slots(
    pane: &Pane<PaneSlot>,
    layout: DockLayout,
    tab: &Tab,
    app: &AgenttyApp,
    window: &Window,
    cx: &App,
    out: &mut Vec<PaneIdentity>,
) {
    match pane {
        Pane::Empty => {}
        Pane::Leaf(_) => {
            if layout == DockLayout::PerColumn {
                if let Some(target) = app.composer_target_for_subtree(
                    pane,
                    tab.last_focused_entity_in_subtree(pane),
                    window,
                    cx,
                ) {
                    out.push(target);
                }
            }
        }
        Pane::Split { axis, a, b, .. } => match axis {
            Axis::Horizontal => {
                collect_dock_slots(a, DockLayout::PerColumn, tab, app, window, cx, out);
                collect_dock_slots(b, DockLayout::PerColumn, tab, app, window, cx, out);
            }
            Axis::Vertical => {
                if let Some(target) = app.composer_target_for_subtree(
                    pane,
                    tab.last_focused_entity_in_subtree(pane),
                    window,
                    cx,
                ) {
                    out.push(target);
                }
            }
        },
    }
}

impl AgenttyApp {
    pub(crate) fn composer_target_for_subtree(
        &self,
        pane: &Pane<PaneSlot>,
        last_focused_in_subtree: Option<gpui::EntityId>,
        window: &Window,
        cx: &App,
    ) -> Option<PaneIdentity> {
        let environment = crate::core::session::WorkspaceStore::environment_id(cx, self.workspace);
        if let Some(target) = self.composer_input_target_in_subtree(pane, window, cx) {
            return Some(target);
        }
        if let Some(terminal) = pane
            .focused_leaf(window, cx)
            .and_then(|slot| slot.terminal().cloned())
        {
            return Some(PaneIdentity {
                environment: environment.clone(),
                pane_id: terminal.read(cx).pane_id(),
            });
        }
        if let Some(id) = last_focused_in_subtree {
            if let Some(terminal) = pane
                .leaf_matching_or_first(|slot| slot.entity_id() == id)
                .and_then(|slot| slot.terminal().cloned())
            {
                return Some(PaneIdentity {
                    environment: environment.clone(),
                    pane_id: terminal.read(cx).pane_id(),
                });
            }
        }
        pane.first_leaf()
            .and_then(|slot| slot.terminal().cloned())
            .map(|terminal| PaneIdentity {
                environment,
                pane_id: terminal.read(cx).pane_id(),
            })
    }

    fn composer_input_target_in_subtree(
        &self,
        pane: &Pane<PaneSlot>,
        window: &Window,
        cx: &App,
    ) -> Option<PaneIdentity> {
        let pane_ids: HashSet<u64> = pane
            .terminals()
            .into_iter()
            .map(|terminal| terminal.read(cx).pane_id())
            .collect();
        for state in self.composers.values() {
            if !pane_ids.contains(&state.target.pane_id) {
                continue;
            }
            if state
                .input
                .read(cx)
                .focus_handle(cx)
                .contains_focused(window, cx)
            {
                return Some(state.target.clone());
            }
        }
        None
    }

    pub(crate) fn render_pane_with_docks(
        &mut self,
        tab_index: usize,
        dim_inactive: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let slots = {
            let tab = &self.tabs[tab_index];
            dock_slots_for_tab(tab, self, window, cx)
        };
        self.sync_composer_docks(&slots, window, cx);
        let pane = self.tabs[tab_index].pane.clone();
        render_pane_node(
            self,
            tab_index,
            &pane,
            DockLayout::PerColumn,
            dim_inactive,
            window,
            cx,
        )
        .into_any_element()
    }
}

fn subtree_target(
    app: &AgenttyApp,
    tab_index: usize,
    pane: &Pane<PaneSlot>,
    window: &Window,
    cx: &App,
) -> Option<PaneIdentity> {
    let last_focused = app.tabs[tab_index].last_focused_entity_in_subtree(pane);
    app.composer_target_for_subtree(pane, last_focused, window, cx)
}

fn render_pane_node(
    app: &mut AgenttyApp,
    tab_index: usize,
    pane: &Pane<PaneSlot>,
    layout: DockLayout,
    dim_inactive: bool,
    window: &mut Window,
    cx: &mut Context<AgenttyApp>,
) -> gpui::AnyElement {
    if layout == DockLayout::TerminalsOnly {
        return div()
            .size_full()
            .child(pane.render(dim_inactive, window, cx))
            .into_any_element();
    }

    match pane {
        Pane::Empty => div().into_any_element(),
        Pane::Leaf(_) => {
            let target = subtree_target(app, tab_index, pane, window, cx);
            div()
                .size_full()
                .flex()
                .flex_col()
                .min_h_0()
                .min_w_0()
                .child(div().flex_1().min_h_0().min_w_0().child(pane.render(
                    dim_inactive,
                    window,
                    cx,
                )))
                .children(dock_elements(app, target, window, cx))
                .into_any_element()
        }
        Pane::Split {
            axis,
            a,
            b,
            ratio,
            dragging,
        } => match axis {
            Axis::Horizontal => render_resizable_split(
                *axis,
                ratio.clone(),
                dragging.clone(),
                render_pane_node(
                    app,
                    tab_index,
                    a,
                    DockLayout::PerColumn,
                    dim_inactive,
                    window,
                    cx,
                ),
                render_pane_node(
                    app,
                    tab_index,
                    b,
                    DockLayout::PerColumn,
                    dim_inactive,
                    window,
                    cx,
                ),
                cx,
            ),
            Axis::Vertical => {
                let target = subtree_target(app, tab_index, pane, window, cx);
                div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .min_h_0()
                    .min_w_0()
                    .child(div().flex_1().min_h_0().min_w_0().child(pane.render(
                        dim_inactive,
                        window,
                        cx,
                    )))
                    .children(dock_elements(app, target, window, cx))
                    .into_any_element()
            }
        },
    }
}

fn dock_elements(
    app: &mut AgenttyApp,
    target: Option<PaneIdentity>,
    window: &mut Window,
    cx: &mut Context<AgenttyApp>,
) -> Vec<gpui::AnyElement> {
    let Some(target) = target else {
        return Vec::new();
    };
    if !app.composer_footer_visible(&target, cx) {
        return Vec::new();
    }
    let mut out = Vec::new();
    if app.composer_dock_expanded(&target, cx) {
        if let Some(composer) = app.render_composer_for(&target, cx) {
            out.push(composer.into_any_element());
        }
    }
    if let Ok(terminal) = app.target_terminal(&target, cx) {
        out.push(
            app.render_composer_context_footer(&terminal, &target, window, cx)
                .into_any_element(),
        );
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn horizontal_split_dock_reuses_resizable_split_divider() {
        let source = include_str!("composer_dock.rs");
        let production = source.split("#[cfg(test)]").next().unwrap_or(source);
        assert!(
            production.contains("render_resizable_split"),
            "H-split dock columns must reuse Pane::render_resizable_split"
        );
        assert!(
            !production.contains("fn split_divider"),
            "static chrome-only divider must not replace the canonical drag path"
        );
        let pane = include_str!("pane.rs");
        assert!(
            pane.contains("pub(crate) fn render_resizable_split"),
            "canonical resizable split must live in pane.rs"
        );
        assert!(
            pane.contains("MouseMoveEvent") && pane.contains("ratio.set"),
            "resizable split must update the ratio Cell while dragging"
        );
    }
}
