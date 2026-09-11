use gpui::{
    Animation, AnimationExt as _, AnyElement, Axis, Bounds, Context, Div, FontWeight, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, SharedString, Stateful, Window, canvas,
    deferred, div, ease_out_quint, linear_color_stop, linear_gradient, prelude::*, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::Input;
use gpui_component::menu::{ContextMenu, ContextMenuExt as _};
use gpui_component::{ActiveTheme as _, Icon, IconName, Sizable as _, h_flex, v_flex};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use std::path::PathBuf;

use crate::core::config::Config;
use crate::ui::app::{TITLE_BAR_HEIGHT, Tab, Tty7App};
use crate::ui::hints::tab_badge_label;
use crate::ui::i18n::{L10nKey, t, t_fmt};
use crate::ui::reorder::{self, Reorder, Surface};
use crate::ui::right_panel::RESIZE_HANDLE_WIDTH;
use crate::ui::tab_strip::{
    DragTab, REORDER_SLIDE_MS, abbreviate_home, elide_keep_edges, elide_label,
    elide_path_keep_tail, measure_text, strip_host_prefix,
};

pub(crate) const MIN_SIDEBAR_WIDTH: f32 = 180.;

const GRAB_HANDLE_W: f32 = 48.;

const ROW_GAP: f32 = 2.;

/// The row chrome the text budget has to be measured around. These are the
/// numbers the layout below is built from, not a second guess at it — a row
/// that elides against a budget wider than it really has falls back to CSS
/// truncation, which drops the tail this whole module exists to keep.
mod row_metrics {
    /// `border_r_1` on the sidebar itself.
    pub(super) const BORDER: f32 = 1.;
    /// `px_1` on the scrolling list that holds the rows.
    pub(super) const LIST_PAD: f32 = 4.;
    /// `pl_2` + `pr_2` on the row.
    pub(super) const ROW_PAD: f32 = 8.;
    /// The avatar handed to `tab_avatar`.
    pub(super) const AVATAR: f32 = 18.;
    /// `gap_1p5` between the row's children.
    pub(super) const GAP: f32 = 6.;
    /// The ⌘N badge, when one is shown.
    pub(super) const BADGE: f32 = 20.;
    /// The zoom mark, when the tab has a pane zoomed over the others.
    pub(super) const ZOOM: f32 = 16.;
    /// `gap_1p5`, between the branch icon and its text and before the counts.
    pub(super) const META_GAP: f32 = 6.;
    /// The branch icon.
    pub(super) const BRANCH_ICON: f32 = 11.;

    /// What a row can spend on text, before the badge is taken out.
    pub(super) const fn text_budget(width: f32) -> f32 {
        width - BORDER - 2. * LIST_PAD - 2. * ROW_PAD - AVATAR - GAP
    }
}

/// What a sidebar row rendered, next to what it had to leave out, so the
/// hover card can be built by comparison instead of deriving the same strings
/// a second time — the two derivations have to agree, and the shortest way to
/// guarantee that is to only ever have one.
struct SidebarRowShown {
    /// The elided title, and the full string it came from. `None` when the
    /// row is showing a placeholder (`Shell 3`) rather than a real title,
    /// which nothing can expand.
    title: Option<(SharedString, SharedString)>,
    branch: Option<(SharedString, SharedString, u32, u32)>,
    cwd: Option<(SharedString, SharedString)>,
}

/// Every detail a sidebar row could not fit, collected so the hover card can
/// be rendered from cloneable data (an `AnyElement` cannot be cloned, but the
/// tooltip closure has to rebuild its content on every hover).
#[derive(Clone)]
struct SidebarInfo {
    /// Full path, when the row's title was elided.
    title: Option<SharedString>,
    /// Full branch plus diff counts, when the row's branch was elided.
    branch: Option<(SharedString, u32, u32)>,
    /// Full working directory, when the row's second line was elided.
    cwd: Option<SharedString>,
    /// Remote host, when the avatar only shows a dot for it.
    host: Option<SharedString>,
}

impl Tty7App {
    /// Whether the tab rail is on screen — the same three conditions `render`
    /// assembles the layout from, in one place the panel opposite can ask.
    pub(crate) fn sidebar_open(&self, cx: &gpui::App) -> bool {
        cx.global::<Config>().tab_bar_position == crate::core::config::TabBarPosition::Left
            && !self.sidebar_collapsed
    }

    /// What the right panel has reserved, from the sidebar's point of view.
    pub(crate) fn right_panel_floor(&self, cx: &gpui::App) -> f32 {
        if self.right_panel_open(cx) {
            crate::ui::right_panel::MIN_WIDTH
        } else {
            0.
        }
    }

    pub(crate) fn sidebar_max_px(&self, window: &Window, cx: &gpui::App) -> f32 {
        crate::ui::app::side_panel_max(
            window.viewport_size().width.as_f32(),
            MIN_SIDEBAR_WIDTH,
            self.right_panel_floor(cx) + self.document_floor(cx),
        )
    }

    /// How wide the sidebar is drawn, given the live cell and the cap the rest
    /// of the window leaves it. Read here rather than clamped at each caller so
    /// the document column's budget and the sidebar itself can never disagree
    /// about how much width is already spoken for.
    pub(crate) fn sidebar_px(&self, window: &Window, cx: &gpui::App) -> f32 {
        self.sidebar_width
            .get()
            .clamp(MIN_SIDEBAR_WIDTH, self.sidebar_max_px(window, cx))
    }

    pub(crate) fn tab_sidebar(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let active = self.active;
        let sf = cx.global::<crate::ui::presets::Surfaces>().sidebar;
        let show_badges = self.mod_hint_badges;
        let width = self.sidebar_px(window, cx);
        let query = self.sidebar_search.read(cx).value().trim().to_lowercase();
        // Blanked here, written again from paint: a row filtered out by the
        // search — or hidden with its collapsed machine — must leave no rectangle
        // behind for a pane to be dropped between.
        *self.sidebar_slots.borrow_mut() = vec![Bounds::default(); self.tabs.len()];
        let mut list = v_flex()
            .id("tab-sidebar-list")
            .flex_shrink_0()
            .w_full()
            .px_1()
            .py_1p5()
            .gap_0p5();

        // Badges and numeric activation share canonical tab order. Search
        // hides rows without renumbering their shortcuts.
        let badge_pos: Vec<usize> = {
            let mut pos = vec![0usize; self.tabs.len()];
            for (n, i) in self.visual_tab_order(cx).into_iter().enumerate() {
                pos[i] = n;
            }
            pos
        };

        // The row shows an elided title and a branch; the filter used to read
        // only the elided title, so typing the branch you can see, or the part
        // of the path the row dropped, matched nothing. The label is built
        // here only when there is a query to match it against — the rows
        // themselves elide against measured width and no longer need it.
        let visible_tabs = self.sidebar_visible_tabs(&query, window, cx);
        let any_rows = !visible_tabs.is_empty();

        let pointer = window.mouse_position();
        // The row text is measured against real glyphs before it is elided:
        // `text_sm` is 0.875rem and `text_xs` 0.75rem, resolved here so the
        // measurement and the render use the same sizes and family.
        let font = gpui::Font {
            family: cx.theme().font_family.clone(),
            features: Default::default(),
            fallbacks: None,
            weight: Default::default(),
            style: Default::default(),
        };
        // The active row renders its title at `FontWeight::MEDIUM`, which is
        // wider than the regular weight in any proportional face. Measuring
        // it as regular would let the one row the user is looking at overflow
        // into the truncation this is here to avoid.
        let title_font_active = gpui::Font {
            weight: FontWeight::MEDIUM,
            ..font.clone()
        };
        let rem = window.rem_size().as_f32();
        // The diff counts in their resting weight: green still means added,
        // but twelve of them down a column no longer outshout the titles.
        let added_ink = crate::ui::presets::resting_ink(
            cx.theme().success,
            cx.theme().muted_foreground,
            cx.theme().sidebar,
        );
        let removed_ink = crate::ui::presets::resting_ink(
            cx.theme().danger,
            cx.theme().muted_foreground,
            cx.theme().sidebar,
        );
        let recoveries = super::session_recovery::for_workspace(cx, self.workspace);
        if any_rows {
            let mut rows: Vec<ContextMenu<Stateful<Div>>> = Vec::new();
            // Full tab order protects index mappings even when hidden tabs change.
            // Rc keeps per-row handlers from copying the identity vectors.
            let row_surface = Rc::new(Surface::SidebarRows {
                tabs: self.tabs.iter().map(|tab| tab.tree_id.get()).collect(),
                visible: visible_tabs.clone(),
            });
            let row_slots: Rc<RefCell<Vec<Bounds<Pixels>>>> =
                Rc::new(RefCell::new(vec![Bounds::default(); visible_tabs.len()]));
            let row_preview = reorder::preview(
                &self.reorder,
                row_surface.as_ref(),
                visible_tabs.len(),
                pointer,
            );
            for (slot, i) in visible_tabs.iter().copied().enumerate() {
                let badge_pos = badge_pos[i];
                let tab = &self.tabs[i];
                let is_active = i == active;
                let ssh_dot = self.tab_ssh_dot(tab, cx);
                let agent = tab.agent(cx);
                let agent_status = tab.agent_status(cx);
                let agent_unread = tab.agent_unread_count(cx);
                let git_cwd = git_click(tab, window, cx);
                let badge_extra = if show_badges && badge_pos < 9 {
                    row_metrics::BADGE + row_metrics::GAP
                } else {
                    0.
                };
                let zoomed = self.tab_is_zoomed(i);
                let zoom_extra = if zoomed {
                    row_metrics::ZOOM + row_metrics::GAP
                } else {
                    0.
                };
                // Elision is measured against this budget so the label and
                // branch never wrap or overflow into CSS truncation.
                let label_avail =
                    (row_metrics::text_budget(width) - badge_extra - zoom_extra).max(48.);
                let title_size = 0.875 * rem;
                let meta_size = 0.75 * rem;
                let title_font = if is_active { &title_font_active } else { &font };
                // Title: elide the *full* label against the row budget, so a
                // wide sidebar shows the whole thing and a narrow one keeps
                // whichever end identifies it — the tail for a path, both
                // edges for anything else. A fixed segment cap
                // (`short_title`) would elide even when the row has room, so
                // only the width may decide here.
                //
                // `full_title` is the unelided string the card can expand
                // back to; `None` means the row is showing a placeholder that
                // no card can improve on.
                let (shown_title, full_title) =
                    if let Some(name) = tab.name.as_ref().filter(|n| !n.trim().is_empty()) {
                        // A renamed tab is elided like anything else — and so
                        // the card has to be able to spell the name back out.
                        let full = SharedString::from(name.trim().to_string());
                        let shown = elide_label(
                            &window.text_system(),
                            title_font,
                            title_size,
                            &full,
                            label_avail,
                        );
                        (shown, Some(full))
                    } else {
                        // The ladder the strip and the switcher climb, read
                        // here for the name and not for the shortening: this
                        // column measures in pixels and lets a card expand the
                        // row back to the whole string, so it wants what
                        // `label_of` would have cut down rather than the cut.
                        use crate::ui::machine_mirror::TabLabel;
                        let (view, home) = tab.label_view(Some(window), cx);
                        let raw = match view.label() {
                            TabLabel::Osc(title) | TabLabel::Cwd(title) => {
                                abbreviate_home(strip_host_prefix(title.trim()), home.as_deref())
                                    .into_owned()
                            }
                            TabLabel::Agent(agent) => agent.display_name().to_string(),
                            // A tab holding a name got one above.
                            TabLabel::Named(name) | TabLabel::Provider(name) => name.to_string(),
                            TabLabel::Process(title) => title.to_string(),
                            TabLabel::Unknown => String::new(),
                        };
                        if raw.trim().is_empty() {
                            // Nothing to expand: the row is naming an unnamed
                            // shell, not hiding a title behind an ellipsis.
                            let placeholder = SharedString::from(t_fmt(
                                L10nKey::TabUnnamedShell,
                                &[("n", &((i + 1).to_string()))],
                            ));
                            (placeholder, None)
                        } else {
                            let full = SharedString::from(raw);
                            let shown = elide_label(
                                &window.text_system(),
                                title_font,
                                title_size,
                                &full,
                                label_avail,
                            );
                            (shown, Some(full))
                        }
                    };
                let mut branch_shown: Option<(SharedString, SharedString, u32, u32)> = None;
                let mut cwd_shown: Option<(SharedString, SharedString)> = None;
                let git_line = tab.git_status(Some(window), cx).map(|g| {
                    let mut line = h_flex()
                        .id(("sidebar-git", i))
                        .w_full()
                        .items_center()
                        .gap_1p5()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(
                            gpui::svg()
                                .path("icons/git-branch.svg")
                                .flex_shrink_0()
                                .size(px(row_metrics::BRANCH_ICON))
                                .text_color(cx.theme().muted_foreground),
                        );
                    // The diff counts are measured against real glyphs so the
                    // branch can be elided to exactly the space they leave;
                    // the counts themselves never wrap or shrink. They render
                    // as two children of a `gap_1p5` row, so the gap between
                    // them is measured rather than a space that stands in for
                    // it.
                    let mut counts_w = 0.;
                    if g.added > 0 {
                        counts_w += measure_text(
                            &window.text_system(),
                            &font,
                            meta_size,
                            &format!("+{}", g.added),
                        );
                    }
                    if g.removed > 0 {
                        counts_w += measure_text(
                            &window.text_system(),
                            &font,
                            meta_size,
                            &format!("−{}", g.removed),
                        );
                    }
                    if g.added > 0 && g.removed > 0 {
                        counts_w += row_metrics::META_GAP;
                    }
                    if counts_w > 0. {
                        // The gap between the branch and the counts.
                        counts_w += row_metrics::META_GAP;
                    }
                    // Branch: keep both ends (`window-…backdrop`) so its
                    // identifying tail survives a narrow sidebar.
                    let branch_avail =
                        (label_avail - row_metrics::BRANCH_ICON - row_metrics::META_GAP - counts_w)
                            .max(0.);
                    let shown = elide_keep_edges(
                        &window.text_system(),
                        &font,
                        meta_size,
                        &g.branch,
                        branch_avail,
                    );
                    branch_shown = Some((
                        shown.clone(),
                        SharedString::from(g.branch.clone()),
                        g.added,
                        g.removed,
                    ));
                    line = line.child(div().flex_1().min_w_0().truncate().child(shown));
                    if g.added > 0 || g.removed > 0 {
                        let mut counts = h_flex()
                            .id(("sidebar-diff", i))
                            .flex_shrink_0()
                            .items_center()
                            .gap_1p5()
                            .when_some(git_cwd, |counts, (host, cwd)| {
                                // A click target inside a click target: the row
                                // highlights as a whole, which says nothing
                                // about the counts being their own button. The
                                // underline the SFTP breadcrumb uses for
                                // clickable text says where this one starts.
                                counts
                                    .cursor_pointer()
                                    .hover(|s| s.underline())
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                                            cx.stop_propagation();
                                            // Swallowing the press also swallows the
                                            // row's click, the only thing that
                                            // activates a tab — so this row has to
                                            // activate itself, or the overlay lands
                                            // in whichever tab was already on
                                            // screen, carrying this row's repo (#706).
                                            this.activate(i, window, cx);
                                            this.toggle_diff_overlay(host, cwd.clone(), window, cx);
                                        }),
                                    )
                            });
                        if g.added > 0 {
                            counts = counts
                                .child(div().text_color(added_ink).child(format!("+{}", g.added)));
                        }
                        if g.removed > 0 {
                            counts = counts.child(
                                div()
                                    .text_color(removed_ink)
                                    .child(format!("−{}", g.removed)),
                            );
                        }
                        line = line.child(counts);
                    }
                    line
                });
                // Outside a repo there is no branch line; the second line then
                // carries the compressed cwd with its root marker, so a tab
                // whose title is just a shell name still says where it lives.
                if git_line.is_none() {
                    cwd_shown = tab
                        .pane
                        .focused_or_first(window, cx)
                        .and_then(|leaf| {
                            let leaf = leaf.read(cx);
                            Some((leaf.effective_cwd()?, leaf.display_home(cx)))
                        })
                        .map(|(cwd, home)| {
                            let text = cwd.display().to_string();
                            let full = SharedString::from(
                                abbreviate_home(&text, home.as_deref()).into_owned(),
                            );
                            let shown = elide_path_keep_tail(
                                &window.text_system(),
                                &font,
                                meta_size,
                                &full,
                                label_avail,
                            );
                            (shown, full)
                        })
                        // The title already carries the whole path; a second
                        // copy adds noise, not information.
                        .filter(|(shown, _)| shown.as_ref() != shown_title.as_ref());
                }
                let rename_input = self
                    .renaming
                    .as_ref()
                    .filter(|r| r.tab == tab.tree_id.get())
                    .map(|r| r.input.clone());

                let shown = SidebarRowShown {
                    title: full_title.map(|full| (shown_title.clone(), full)),
                    branch: branch_shown.clone(),
                    cwd: cwd_shown.clone(),
                };
                let info = self.sidebar_info(tab, window, cx, &shown);
                // Colors are captured by value so the tooltip builder (which
                // borrows no app state) can style the card on its own.
                let muted = cx.theme().muted_foreground;
                let success = added_ink;
                let danger = removed_ink;

                let label_region = match rename_input {
                    Some(input) => div()
                        .id(("sidebar-rename", i))
                        .flex_1()
                        .min_w_0()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        // The row switches tabs on the *release* now, so
                        // holding the press back is no longer enough: a click
                        // landing in the field would reach the row behind it
                        // and switch away from the name being typed, taking
                        // the focus with it.
                        .on_click(|_, _, cx| cx.stop_propagation())
                        .child(Input::new(&input).appearance(false))
                        .into_any_element(),
                    None => v_flex()
                        .id(("sidebar-label", i))
                        .flex_1()
                        .min_w_0()
                        .gap(px(2.))
                        .when_some(info, |col, info| {
                            col.tooltip(move |window, cx| {
                                // `Tooltip::element` rebuilds its content on
                                // every hover, so the captured info is cloned
                                // per call instead of being moved out.
                                let info = info.clone();
                                gpui_component::tooltip::Tooltip::element(move |_window, _cx| {
                                    let card = v_flex()
                                        .gap_1()
                                        // The card is the one place that
                                        // promised the whole string, so a long
                                        // path wraps here rather than being
                                        // truncated a second time.
                                        .when_some(info.title.clone(), |c, title| {
                                            c.child(
                                                div()
                                                    .max_w(px(420.))
                                                    .text_sm()
                                                    .font_weight(FontWeight::MEDIUM)
                                                    .child(title),
                                            )
                                        })
                                        .when_some(
                                            info.branch.clone(),
                                            |c, (branch, added, removed)| {
                                                let mut line = h_flex()
                                                    .items_center()
                                                    .gap_1p5()
                                                    .text_xs()
                                                    .text_color(muted)
                                                    .child(
                                                        gpui::svg()
                                                            .path("icons/git-branch.svg")
                                                            .flex_shrink_0()
                                                            .size(px(11.))
                                                            .text_color(muted),
                                                    )
                                                    .child(div().child(branch));
                                                if added > 0 {
                                                    line = line.child(
                                                        div()
                                                            .text_color(success)
                                                            .child(format!("+{added}")),
                                                    );
                                                }
                                                if removed > 0 {
                                                    line = line.child(
                                                        div()
                                                            .text_color(danger)
                                                            .child(format!("−{removed}")),
                                                    );
                                                }
                                                c.child(line)
                                            },
                                        )
                                        .when_some(info.cwd.clone(), |c, cwd| {
                                            c.child(
                                                div()
                                                    .max_w(px(420.))
                                                    .text_xs()
                                                    .text_color(muted)
                                                    .child(cwd),
                                            )
                                        })
                                        .when_some(info.host.clone(), |c, host| {
                                            c.child(
                                                h_flex()
                                                    .items_center()
                                                    .gap_1p5()
                                                    .text_xs()
                                                    .text_color(muted)
                                                    .child(
                                                        gpui::svg()
                                                            .path("icons/machine-remote.svg")
                                                            .flex_shrink_0()
                                                            .size(px(11.))
                                                            .text_color(muted),
                                                    )
                                                    .child(div().truncate().child(host)),
                                            )
                                        });
                                    card
                                })
                                .build(window, cx)
                            })
                        })
                        .child(
                            div()
                                .w_full()
                                .truncate()
                                .text_sm()
                                .when(is_active, |d| d.font_weight(FontWeight::MEDIUM))
                                .child(shown_title),
                        )
                        .children(git_line)
                        .when_some(cwd_shown, |col, (cwd, _)| {
                            col.child(
                                h_flex()
                                    .id(("sidebar-cwd", i))
                                    .w_full()
                                    .items_center()
                                    .gap_1p5()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(div().flex_1().min_w_0().truncate().child(cwd)),
                            )
                        })
                        .into_any_element(),
                };

                let row = h_flex()
                    .id(("tab-row", i))
                    .group(SharedString::from(format!("tab-row-{i}")))
                    .cursor_pointer()
                    .on_drag(DragTab, {
                        let state = self.reorder.clone();
                        let slots = row_slots.clone();
                        let row_surface = row_surface.clone();
                        let id = tab.tree_id.get();
                        move |_drag, grab, _window, cx| {
                            cx.stop_propagation();
                            *state.borrow_mut() = Some(
                                Reorder::new(
                                    (*row_surface).clone(),
                                    slot,
                                    slots.borrow().clone(),
                                    Axis::Vertical,
                                    px(ROW_GAP),
                                    grab,
                                )
                                .of_tab(id),
                            );
                            cx.new(|_| DragTab)
                        }
                    })
                    .w_full()
                    .py_0p5()
                    .items_center()
                    .justify_between()
                    .gap_1p5()
                    .pl_2()
                    .pr_2()
                    .rounded_lg()
                    .when(is_active, |s| {
                        s.bg(cx.theme().sidebar_accent)
                            .text_color(cx.theme().sidebar_accent_foreground)
                    })
                    .when(!is_active, |s| {
                        s.text_color(cx.theme().sidebar_foreground)
                            .hover(|s| s.bg(gpui::rgb(sf.hover)))
                    })
                    .when(row_preview.as_ref().is_some_and(|p| p.from == slot), |s| {
                        s.opacity(0.75)
                    })
                    .child(
                        canvas(
                            {
                                let slots = row_slots.clone();
                                // The row by tab as well as by slot: reordering
                                // reads visible slots, while a pane dropped
                                // on the sidebar reads every row there is.
                                let by_tab = self.sidebar_slots.clone();
                                move |bounds, _window, _cx| {
                                    if let Some(s) = slots.borrow_mut().get_mut(slot) {
                                        *s = bounds;
                                    }
                                    if let Some(s) = by_tab.borrow_mut().get_mut(i) {
                                        *s = bounds;
                                    }
                                }
                            },
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .inset_0(),
                    )
                    // Switched on the release, not the press: a press that turns
                    // into a drag is the tab being picked up, and a tab on its
                    // way into another tab's layout must not put itself on
                    // screen on the way there.
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.activate(i, window, cx);
                    }))
                    .child(self.tab_avatar(
                        ("sidebar-avatar", i),
                        agent,
                        agent_status,
                        agent_unread,
                        ssh_dot,
                        is_active,
                        row_metrics::AVATAR,
                        cx,
                    ))
                    // Leading, like the chip's: the trailing end of a row is
                    // the badge's, and the close button fades in over it.
                    .when(zoomed, |row| {
                        row.child(self.zoom_mark(("sidebar-zoom", i), cx))
                    })
                    .child(label_region)
                    .children(
                        recoveries
                            .get(&tab.tree_id.get())
                            .filter(|_| agent.is_none())
                            .map(|recovery| super::session_recovery::badge(recovery, cx)),
                    )
                    .when(show_badges && badge_pos < 9, |row| {
                        row.child(
                            div()
                                .flex_shrink_0()
                                .flex()
                                .items_center()
                                .justify_center()
                                .size(px(20.))
                                .text_xs()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(if is_active {
                                    cx.theme().sidebar_accent_foreground
                                } else {
                                    cx.theme().muted_foreground
                                })
                                .child(tab_badge_label(badge_pos)),
                        )
                    })
                    .when(!(show_badges && badge_pos < 9), |row| {
                        let backing: gpui::Hsla = if is_active {
                            gpui::rgb(sf.selected).into()
                        } else {
                            gpui::rgb(sf.hover).into()
                        };
                        let mut fade_from = backing;
                        fade_from.a = 0.;
                        row.child(
                            h_flex()
                                .absolute()
                                .top(px(4.))
                                .right(px(6.))
                                .opacity(0.)
                                .group_hover(SharedString::from(format!("tab-row-{i}")), |s| {
                                    s.opacity(1.)
                                })
                                .child(div().w(px(10.)).h(px(crate::ui::tab_strip::MIN_TARGET)).bg(
                                    linear_gradient(
                                        90.,
                                        linear_color_stop(fade_from, 0.),
                                        linear_color_stop(backing, 1.),
                                    ),
                                ))
                                .child(
                                    div().bg(backing).child(
                                        crate::ui::tab_strip::hit_target(
                                            Button::new(("sidebar-close", i))
                                                .icon(IconName::Close)
                                                .ghost()
                                                .xsmall(),
                                        )
                                        .tooltip(t(L10nKey::TabContextCloseTab))
                                        // Held here, because the row behind it
                                        // switches tabs on the release too:
                                        // without this the same click closes
                                        // tab `i` and then activates whichever
                                        // tab slid into its place.
                                        .on_click(
                                            cx.listener(move |this, _, window, cx| {
                                                cx.stop_propagation();
                                                this.close_tab(i, window, cx);
                                            }),
                                        ),
                                    ),
                                ),
                        )
                    });

                let menu_app = cx.entity().downgrade();
                rows.push(row.context_menu(move |menu, window, cx| {
                    Tty7App::tab_context_menu(menu, i, true, &menu_app, window, cx)
                }));
            }

            let row_display: Vec<usize> = match &row_preview {
                Some(p) => {
                    if let Some(order) =
                        reordered_rows(self.tabs.len(), &visible_tabs, p.from, p.target)
                    {
                        reorder::set_pending(&self.reorder, row_surface.as_ref(), order);
                    }
                    p.order.clone()
                }
                None => (0..rows.len()).collect(),
            };
            let mut rows: Vec<Option<ContextMenu<Stateful<Div>>>> =
                rows.into_iter().map(Some).collect();
            let rows: Vec<AnyElement> = row_display
                .into_iter()
                .map(|slot| match &row_preview {
                    Some(p) if p.from == slot => deferred(
                        rows[slot]
                            .take()
                            .expect("each slot emitted once")
                            .relative()
                            .top(p.held),
                    )
                    .into_any_element(),
                    Some(p) => {
                        let offset = p.offsets[slot].as_f32();
                        rows[slot]
                            .take()
                            .expect("each slot emitted once")
                            .with_animation(
                                (
                                    SharedString::from(format!("row-slide-{}", p.generation)),
                                    slot,
                                ),
                                Animation::new(std::time::Duration::from_millis(REORDER_SLIDE_MS))
                                    .with_easing(ease_out_quint()),
                                move |el, delta| el.top(px(offset * (1. - delta))),
                            )
                            .into_any_element()
                    }
                    None => rows[slot]
                        .take()
                        .expect("each slot emitted once")
                        .into_any_element(),
                })
                .collect();

            list = list.child(v_flex().w_full().gap(px(ROW_GAP)).children(rows));
        }

        if !any_rows && !query.is_empty() {
            list = list.child(
                div()
                    .px_2()
                    .py_3()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(crate::ui::i18n::t_fmt(
                        crate::ui::i18n::L10nKey::SettingsNothingMatches,
                        &[("query", &query)],
                    )),
            );
        }

        let list = self.render_machine_rail(list.into_any_element(), &query, cx);

        // Empty chrome until the pointer is over the rail: these two tiles are
        // the only thing between a resting window and a bare column of tabs.
        let chrome_shown = self.sidebar_chrome_hover.get();
        let controls = h_flex()
            .flex_shrink_0()
            .h(px(TITLE_BAR_HEIGHT))
            .border_b_1()
            .border_color(cx.theme().transparent)
            .items_center()
            .justify_end()
            .gap(px(2.))
            .pr(px(crate::ui::app::tile_trailing_inset()))
            .when_some(crate::ui::app::window_mark(), |row, mark| {
                row.child(
                    div()
                        .flex_shrink_0()
                        .pl(px(crate::ui::app::CONTENT_INSET))
                        .child(mark),
                )
                .child(div().flex_1().min_w(px(GRAB_HANDLE_W)))
            })
            .child(
                div()
                    .occlude()
                    .flex_shrink_0()
                    .when(!chrome_shown, |tile| tile.invisible())
                    .child(self.new_tab_button("sidebar-add", cx)),
            )
            .child(
                div()
                    .occlude()
                    .flex_shrink_0()
                    .when(!chrome_shown, |tile| tile.invisible())
                    .child(
                        crate::ui::tab_strip::chrome_tile(
                            Button::new("sidebar-collapse")
                                .icon(Icon::empty().path("icons/panel-left.svg")),
                            false,
                            cx,
                        )
                        .rounded_lg()
                        .tooltip_element(crate::ui::tab_strip::chord_tooltip(
                            t(L10nKey::TabTooltipHideSidebar),
                            "ToggleLeftPanel",
                            cx,
                        ))
                        .on_click(cx.listener(|this, _, _window, cx| this.toggle_left_panel(cx))),
                    ),
            );
        // The tile inside asks for `w_full`, and a percentage is only a width
        // while some box above it has a real one. This row used to have none of
        // its own and borrowed the column's by cross-axis stretch, which did not
        // always hold; `w_full` here swapped that for a second percentage, and a
        // row whose width is `Percent` is no longer `auto`, so it lost stretch
        // as well — on the passes that size the column from its content there
        // was still nothing to resolve against and the tile fell back to hugging
        // the workspace name. Hand the row real pixels: the rail is
        // `w(px(width))` and layout is border-box, so its content is one pixel
        // narrower than that because of the right border.
        let chip_inset = crate::ui::app::CONTENT_INSET - 7. + 4.;
        let top_bar = h_flex()
            .flex_shrink_0()
            .items_center()
            .gap(px(6.))
            .h(px(44.))
            .pl(px(chip_inset))
            .pr(px(crate::ui::app::CONTENT_INSET))
            .child(
                div()
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(Self::AVATAR_PX))
                    .child(
                        Icon::new(IconName::Search)
                            .size(px(14.))
                            .text_color(cx.theme().muted_foreground),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(Input::new(&self.sidebar_search).appearance(false).pl_0()),
            );

        let container: Rc<Cell<Option<Bounds<Pixels>>>> = Rc::new(Cell::new(None));
        // Read while there is still a `cx` to read it from: the drag handler
        // below only ever sees a `Window`, and the cap it clamps against has to
        // be the same one the layout applies or the sidebar springs back from
        // wherever it was dropped.
        let others_floor = self.right_panel_floor(cx) + self.document_floor(cx);
        let backing = canvas(
            {
                let container = container.clone();
                move |bounds, _window, _cx| container.set(Some(bounds))
            },
            {
                let container = container.clone();
                let width_cell = self.sidebar_width.clone();
                let dragging = self.sidebar_dragging.clone();
                move |_bounds, _state, window, _cx| {
                    window.on_mouse_event({
                        let container = container.clone();
                        let width_cell = width_cell.clone();
                        let dragging = dragging.clone();
                        move |ev: &MouseMoveEvent, _phase, window, _cx| {
                            if !dragging.get() {
                                return;
                            }
                            let Some(b) = container.get() else {
                                return;
                            };
                            let raw = (ev.position.x - b.origin.x).as_f32();
                            let max = crate::ui::app::side_panel_max(
                                window.viewport_size().width.as_f32(),
                                MIN_SIDEBAR_WIDTH,
                                others_floor,
                            );
                            width_cell.set(raw.clamp(MIN_SIDEBAR_WIDTH, max));
                            window.refresh();
                        }
                    });
                    window.on_mouse_event({
                        let width_cell = width_cell.clone();
                        let dragging = dragging.clone();
                        move |_ev: &MouseUpEvent, _phase, window, cx| {
                            if !dragging.get() {
                                return;
                            }
                            dragging.set(false);
                            let w = width_cell.get();
                            let cfg = cx.global_mut::<Config>();
                            if cfg.sidebar_width != w {
                                cfg.sidebar_width = w;
                                cfg.save();
                            }
                            window.refresh();
                        }
                    });
                }
            },
        )
        .absolute()
        .size_full();

        let handle_active = self.sidebar_dragging.get();
        let handle = div()
            .group("sidebar-resize")
            .occlude()
            .absolute()
            .top_0()
            .right(px(-(RESIZE_HANDLE_WIDTH / 2.)))
            .w(px(RESIZE_HANDLE_WIDTH))
            .h_full()
            .flex()
            .items_center()
            .justify_center()
            .cursor_col_resize()
            .child(
                div()
                    .w(px(1.))
                    .h_full()
                    .when(handle_active, |d| d.bg(cx.theme().drag_border))
                    .group_hover("sidebar-resize", |s| s.bg(cx.theme().drag_border)),
            )
            .on_mouse_down(MouseButton::Left, {
                let dragging = self.sidebar_dragging.clone();
                move |_ev, window, _cx| {
                    dragging.set(true);
                    window.refresh();
                }
            });

        div()
            .relative()
            .flex_shrink_0()
            .w(px(width))
            .h_full()
            .bg(crate::ui::theme::workspace_surface_color(cx))
            .border_r_1()
            .border_color(cx.theme().sidebar_border)
            .child(backing)
            .child(
                // Real pixels, not `size_full`: the rail's own width is a
                // definite `px`, but a percentage off it is still a percentage,
                // and on the passes that size this column from its content it
                // resolves against nothing. Everything below asks for `w_full`
                // — the tab rows, their group blocks, the scroll area — so one
                // unresolved link here collapsed the whole chain and every row
                // fell back to hugging the longest tab name. Border-box takes
                // the rail's 1px right border off the content width.
                v_flex()
                    .w(px(width - 1.))
                    .h_full()
                    .child(crate::ui::app::title_bar_drag(
                        controls.id("sidebar-titlebar-drag"),
                        "sidebar-titlebar-drag",
                        window,
                        cx,
                    ))
                    .child(top_bar)
                    .child(crate::ui::scrollbar::with_vertical_scrollbar(
                        "tab-sidebar-scrollbar",
                        list,
                        &self.sidebar_scroll,
                    )),
            )
            .child(handle)
            .child(crate::ui::app::hover_sheet(
                "sidebar-chrome-hover",
                &self.sidebar_chrome_hover,
            ))
    }

    /// What the sidebar row hid: the full title, the full branch and diff
    /// counts, the working directory, and the remote host the avatar only
    /// dots. `None` when the row showed everything — a card would add noise,
    /// not information. The host is included even for an untruncated row,
    /// because the title strips the `user@host:` prefix the avatar cannot
    /// spell out.
    ///
    /// Every line is decided by comparing what the row rendered against the
    /// string it was elided from. Both come from the row itself: deriving
    /// them here a second time is how a renamed tab ended up with a name the
    /// row shortened and the card refused to expand.
    fn sidebar_info(
        &self,
        tab: &crate::ui::app::Tab,
        window: &mut Window,
        cx: &gpui::App,
        shown: &SidebarRowShown,
    ) -> Option<SidebarInfo> {
        let elided = |pair: &Option<(SharedString, SharedString)>| {
            pair.as_ref()
                .filter(|(shown, full)| shown != full)
                .map(|(_, full)| full.clone())
        };
        let mut info = SidebarInfo {
            title: elided(&shown.title),
            branch: shown
                .branch
                .as_ref()
                .filter(|(shown, full, _, _)| shown != full)
                .map(|(_, full, added, removed)| (full.clone(), *added, *removed)),
            // The cwd only earns a card line when it was rendered *and*
            // elided: a repo row already shows the full path as its title, so
            // repeating the cwd under it would be noise, not information.
            cwd: elided(&shown.cwd),
            host: None,
        };
        // The host is read off the same leaf the title and cwd came from; a
        // split tab whose panes sit on different machines would otherwise
        // name whichever one happens to be first.
        if let Some(target) = tab.pane.focused_or_first(window, cx).and_then(|leaf| {
            leaf.read(cx)
                .remote_context()
                .map(|r| SharedString::from(r.target.clone()))
        }) {
            info.host = Some(target);
        }
        (info.title.is_some() || info.branch.is_some() || info.cwd.is_some() || info.host.is_some())
            .then_some(info)
    }

    fn sidebar_visible_tabs(&self, query: &str, window: &Window, cx: &gpui::App) -> Vec<usize> {
        (0..self.tabs.len())
            .filter(|&i| {
                query.is_empty()
                    || self
                        .tab_label(&self.tabs[i], i, Some(window), cx)
                        .to_lowercase()
                        .contains(query)
                    || self.tabs[i]
                        .leaf_title(Some(window), cx)
                        .to_lowercase()
                        .contains(query)
                    || self.tabs[i]
                        .git_status(Some(window), cx)
                        .is_some_and(|g| g.branch.to_lowercase().contains(query))
            })
            .collect()
    }

    /// Release can happen after the final drag frame. Recheck the same identity
    /// and search projection before the root consumes the pending index order.
    pub(crate) fn validate_sidebar_reorder(&self, window: &Window, cx: &gpui::App) {
        let state = self.reorder.borrow();
        let Some(drag) = state.as_ref() else { return };
        let Surface::SidebarRows { tabs, visible } = &drag.surface else {
            return;
        };
        let query = self.sidebar_search.read(cx).value().trim().to_lowercase();
        if cx.global::<Config>().tab_bar_position != crate::core::config::TabBarPosition::Left
            || !tabs
                .iter()
                .copied()
                .eq(self.tabs.iter().map(|tab| tab.tree_id.get()))
            || *visible != self.sidebar_visible_tabs(&query, window, cx)
        {
            drop(state);
            reorder::clear_pending(&self.reorder);
        }
    }

    fn visual_tab_order(&self, _cx: &gpui::App) -> Vec<usize> {
        (0..self.tabs.len()).collect()
    }

    pub(crate) fn activate_visual(
        &mut self,
        n: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(&i) = self.visual_tab_order(cx).get(n) {
            self.activate(i, window, cx);
        }
    }
}

/// Move one visible tab across its visible anchor without moving any other
/// tab relative to its neighbors. Search supplies ascending original indices.
fn reordered_rows(total: usize, visible: &[usize], from: usize, to: usize) -> Option<Vec<usize>> {
    if visible.iter().any(|&i| i >= total) || visible.windows(2).any(|pair| pair[0] >= pair[1]) {
        return None;
    }
    let (&moved, &anchor) = (visible.get(from)?, visible.get(to)?);
    if moved == anchor {
        return None;
    }
    let mut order: Vec<usize> = (0..total).collect();
    order.remove(moved);
    order.insert(anchor, moved);
    Some(order)
}

/// Where a click on a tab's diff counts opens the overlay: the focused pane's
/// repo, when the setting allows a preview at all.
fn git_click(
    tab: &Tab,
    window: &Window,
    cx: &gpui::App,
) -> Option<(crate::ui::host_ops::HostId, PathBuf)> {
    diff_click_cwd(
        cx.global::<Config>(),
        tab.pane.focused_or_first(window, cx).and_then(|leaf| {
            let view = leaf.read(cx);
            let cwd = view.git_status_cwd()?.to_path_buf();
            Some((view.host_id(), cwd))
        }),
    )
}

/// Whether a `+N −M` is a button, and what it opens if it is.
///
/// One function because the setting is one setting: the sidebar's counts and
/// the Info panel's `changes` row are the same number about the same working
/// tree, and "Open diff preview from sidebar counts" turning one of them into
/// plain text while the other stayed clickable would be a setting that half
/// works.
pub(crate) fn diff_click_cwd<T>(cfg: &Config, target: Option<T>) -> Option<T> {
    cfg.sidebar_diff_preview.then_some(target).flatten()
}

#[cfg(test)]
mod fold_tests {
    use super::*;
    use crate::core::group_key::GroupKey;
    use crate::ui::app::test_window::harness_with_tabs;
    use gpui::TestAppContext;

    #[gpui::test]
    fn direct_list_release_refuses_stale_order(cx: &mut TestAppContext) {
        let (app, mut vcx, _streams) = harness_with_tabs(cx, 3);
        for change in 0..3 {
            let expected = app.update_in(&mut vcx, |app, window, cx| {
                for i in 0..3 {
                    app.tabs[i].name = Some(format!("release-fixture-{i}"));
                }
                app.sidebar_search
                    .update(cx, |state, cx| state.set_value("", window, cx));
                let surface = Surface::SidebarRows {
                    tabs: app.tabs.iter().map(|tab| tab.tree_id.get()).collect(),
                    visible: vec![0, 1, 2],
                };
                *app.reorder.borrow_mut() = Some(Reorder::new(
                    surface.clone(),
                    0,
                    vec![Bounds::default(); 3],
                    Axis::Vertical,
                    px(ROW_GAP),
                    gpui::point(px(0.), px(0.)),
                ));
                reorder::set_pending(&app.reorder, &surface, vec![1, 2, 0]);
                if change == 1 {
                    app.sidebar_search.update(cx, |state, cx| {
                        state.set_value("release-fixture-2", window, cx);
                    });
                } else if change == 0 {
                    app.tabs.swap(0, 1);
                }
                cx.notify();
                let mut expected = app
                    .tabs
                    .iter()
                    .map(|tab| tab.tree_id.get())
                    .collect::<Vec<_>>();
                if change == 2 {
                    expected.rotate_left(1);
                }
                expected
            });
            vcx.run_until_parked();
            app.update(&mut vcx, |app, _| {
                assert_eq!(
                    app.tabs
                        .iter()
                        .map(|tab| tab.tree_id.get())
                        .collect::<Vec<_>>(),
                    expected
                );
                assert!(
                    app.reorder.borrow().is_none(),
                    "release consumes accepted and refused drags"
                );
            });
        }
    }

    #[gpui::test]
    fn direct_list_numeric_activation_and_search_bounds(cx: &mut TestAppContext) {
        let (app, mut vcx, _streams) = harness_with_tabs(cx, 3);
        app.update_in(&mut vcx, |app, window, cx| {
            for i in 0..3 {
                app.tabs[i].name = Some(format!("direct-list-fixture-{i}"));
                *app.tabs[i].sidebar_group.borrow_mut() =
                    Some(GroupKey::Repo(PathBuf::from(format!("/legacy/{}", 2 - i))));
            }
            assert_eq!(app.visual_tab_order(cx), vec![0, 1, 2]);
            for i in 0..3 {
                assert_eq!(tab_badge_label(i), (i + 1).to_string());
                let id = app.tabs[i].tree_id.get();
                app.activate_visual(i, window, cx);
                assert_eq!(app.tabs[app.active].tree_id.get(), id);
            }
            app.sidebar_search.update(cx, |state, cx| {
                state.set_value("direct-list-fixture-2", window, cx);
            });
            cx.notify();
        });
        vcx.run_until_parked();
        app.update_in(&mut vcx, |app, window, cx| {
            assert!(!drawn(app, 0));
            assert!(!drawn(app, 1));
            assert!(drawn(app, 2));
            assert_eq!(app.visual_tab_order(cx), vec![0, 1, 2]);
            app.activate_visual(2, window, cx);
            assert_eq!(app.active, 2, "search must not renumber shortcut 3");
            app.activate_visual(8, window, cx);
            assert_eq!(app.active, 2, "missing numeric target is a no-op");
            app.sidebar_search.update(cx, |state, cx| {
                state.set_value("", window, cx);
            });
        });
        vcx.run_until_parked();
        app.update(&mut vcx, |app, _| assert!((0..3).all(|i| drawn(app, i))));
    }

    /// Bounds a row registered for itself while it was on screen. A folded
    /// row leaves the default rectangle behind, and that is what stops a pane
    /// being dropped into a group that is shut.
    fn drawn(app: &Tty7App, i: usize) -> bool {
        app.sidebar_slots.borrow()[i].size.height > px(0.)
    }

    #[gpui::test]
    fn folding_a_machine_takes_its_rows_off_the_sidebar(cx: &mut TestAppContext) {
        let (app, mut vcx, _streams) = harness_with_tabs(cx, 3);
        let alpha = GroupKey::Repo(PathBuf::from("/w/alpha"));
        let beta = GroupKey::Repo(PathBuf::from("/w/beta"));

        app.update(&mut vcx, |app, cx| {
            for (i, root) in [(0, &alpha), (1, &alpha), (2, &beta)] {
                *app.tabs[i].sidebar_group.borrow_mut() = Some(root.clone());
            }
            app.active = 2;
            cx.notify();
        });
        vcx.run_until_parked();

        app.update(&mut vcx, |app, _| {
            assert!(
                (0..3).all(|i| drawn(app, i)),
                "every row is on screen before anything is folded"
            );
        });

        app.update(&mut vcx, |app, cx| {
            app.toggle_machine_fold(String::new(), cx)
        });
        vcx.run_until_parked();

        app.update(&mut vcx, |app, cx| {
            assert!(
                (0..3).all(|i| !drawn(app, i)),
                "folding the machine hides all its rows regardless of legacy repo group"
            );
            assert_eq!(app.active, 2, "folding never changes active content");
            assert_eq!(app.tabs.len(), 3, "folding never removes a session");
            assert_eq!(
                cx.global::<Config>().machine_rail.collapsed,
                vec![String::new()],
                "the fold is written where the next launch will read it"
            );
        });

        app.update(&mut vcx, |app, cx| {
            app.toggle_machine_fold(String::new(), cx)
        });
        vcx.run_until_parked();

        app.update(&mut vcx, |app, cx| {
            assert!((0..3).all(|i| drawn(app, i)), "unfolding brings them back");
            assert!(
                cx.global::<Config>().machine_rail.collapsed.is_empty(),
                "and takes the entry back out rather than piling up"
            );
        });
    }

    #[gpui::test]
    fn a_search_outranks_a_machine_fold(cx: &mut TestAppContext) {
        let (app, mut vcx, _streams) = harness_with_tabs(cx, 2);
        let alpha = GroupKey::Repo(PathBuf::from("/w/alpha"));

        app.update(&mut vcx, |app, cx| {
            for i in 0..2 {
                *app.tabs[i].sidebar_group.borrow_mut() = Some(alpha.clone());
            }
            app.toggle_machine_fold(String::new(), cx);
        });
        vcx.run_until_parked();
        app.update(&mut vcx, |app, _| {
            assert!(!drawn(app, 1), "folded, so the row is not drawn");
        });

        // Whatever the row is actually showing — the label is derived from the
        // test process's cwd, and this has to be a query that matches it.
        app.update_in(&mut vcx, |app, window, cx| {
            let label = app.tab_label(&app.tabs[1], 1, Some(window), cx).to_string();
            app.sidebar_search.update(cx, |state, cx| {
                state.set_value(&label, window, cx);
            });
        });
        vcx.run_until_parked();

        app.update(&mut vcx, |app, cx| {
            assert!(
                drawn(app, 1),
                "a row a query matches has to show, fold or no fold"
            );
            assert_eq!(
                cx.global::<Config>().machine_rail.collapsed,
                vec![String::new()],
                "search reveals children without rewriting the saved fold"
            );
        });
    }

    /// A fold hides every row the group has, the active one included. The
    /// alternative — leaving the active row on screen under a shut chevron,
    /// with the header counting rows that are not drawn — looks like a list
    /// that failed to load, which is what folding a group you are working in
    /// used to produce.
    #[gpui::test]
    fn a_machine_fold_hides_the_active_row_too(cx: &mut TestAppContext) {
        let (app, mut vcx, _streams) = harness_with_tabs(cx, 2);
        let alpha = GroupKey::Repo(PathBuf::from("/w/alpha"));

        app.update(&mut vcx, |app, cx| {
            for i in 0..2 {
                *app.tabs[i].sidebar_group.borrow_mut() = Some(alpha.clone());
            }
            app.active = 0;
            app.toggle_machine_fold(String::new(), cx);
        });
        vcx.run_until_parked();

        app.update(&mut vcx, |app, _| {
            assert!(!drawn(app, 0), "the active row folds away with the rest");
            assert!(!drawn(app, 1), "and so does everything else in the group");
        });

        app.update(&mut vcx, |app, cx| {
            app.toggle_machine_fold(String::new(), cx)
        });
        vcx.run_until_parked();

        app.update(&mut vcx, |app, _| {
            assert!((0..2).all(|i| drawn(app, i)), "unfolding brings both back");
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_list_reorder_rejects_invalid_membership() {
        assert_eq!(reordered_rows(3, &[0, 0, 2], 0, 2), None);
        assert_eq!(reordered_rows(3, &[2, 0], 0, 1), None);
        assert_eq!(reordered_rows(3, &[0, 3], 0, 1), None);
    }

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn diff_preview_setting_gates_the_click_target() {
        let mut cfg = Config::default();
        assert!(cfg.sidebar_diff_preview, "default is today's behaviour");
        assert_eq!(
            diff_click_cwd(&cfg, Some(p("/w/repo"))),
            Some(p("/w/repo")),
            "enabled: the counts are a click target"
        );

        cfg.sidebar_diff_preview = false;
        assert_eq!(
            diff_click_cwd(&cfg, Some(p("/w/repo"))),
            None,
            "disabled: no cwd, so no cursor and no toggle_diff_overlay"
        );
    }

    #[test]
    fn diff_click_target_needs_a_repo_either_way() {
        let mut cfg = Config::default();
        assert_eq!(diff_click_cwd::<PathBuf>(&cfg, None), None);
        cfg.sidebar_diff_preview = false;
        assert_eq!(diff_click_cwd::<PathBuf>(&cfg, None), None);
    }

    #[test]
    fn direct_list_reorder_preserves_other_tabs() {
        assert_eq!(reordered_rows(3, &[0, 2], 0, 1), Some(vec![1, 2, 0]));
        assert_eq!(reordered_rows(3, &[0, 2], 1, 0), Some(vec![2, 0, 1]));
        for total in 0..8 {
            for mask in 0..(1usize << total) {
                let visible: Vec<usize> = (0..total).filter(|i| mask & (1 << i) != 0).collect();
                for from in 0..visible.len() {
                    for to in 0..visible.len() {
                        let actual = reordered_rows(total, &visible, from, to);
                        if from == to {
                            assert_eq!(actual, None);
                            continue;
                        }
                        let order = actual.unwrap();
                        let mut sorted = order.clone();
                        sorted.sort_unstable();
                        assert_eq!(sorted, (0..total).collect::<Vec<_>>());
                        let moved = visible[from];
                        assert_eq!(
                            order
                                .iter()
                                .copied()
                                .filter(|&i| i != moved)
                                .collect::<Vec<_>>(),
                            (0..total).filter(|&i| i != moved).collect::<Vec<_>>()
                        );
                        assert_eq!(order[visible[to]], moved);
                    }
                }
            }
        }
        assert_eq!(reordered_rows(0, &[], 0, 1), None);
        assert_eq!(reordered_rows(3, &[0, 2], 2, 0), None);
    }
}
