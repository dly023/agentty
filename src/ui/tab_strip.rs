use crate::core::i18n::ResolveLocale as _;
use gpui::{
    Animation, AnimationExt as _, AnyElement, App, Axis, Bounds, Context, FontWeight, MouseButton,
    Pixels, SharedString, Window, canvas, deferred, div, ease_out_quint, linear_color_stop,
    linear_gradient, prelude::*, px,
};
use gpui_component::InteractiveElementExt as _;
use gpui_component::button::{Button, ButtonCustomVariant, ButtonVariants as _};
use gpui_component::input::Input;
use gpui_component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_component::{ActiveTheme as _, Icon, IconName, Selectable as _, Sizable as _, h_flex};
use std::cell::RefCell;
use std::rc::Rc;

use crate::core::actions::{
    SelectWorkspace1, SelectWorkspace2, SelectWorkspace3, SelectWorkspace4, SelectWorkspace5,
    SelectWorkspace6, SelectWorkspace7, SelectWorkspace8, SelectWorkspace9,
};
use crate::core::config::RightPanelTab;
use crate::daemon::protocol::ShellSpec;
use crate::ui::app::{
    AgenttyApp, TILE_GLYPH, TILE_GLYPH_LINE, TILE_SIZE, Tab, tile_trailing_inset,
};
use crate::ui::hints::tab_badge_label;
use crate::ui::reorder::{self, Reorder, Surface, TabDragHitZone, TabDragIntent};

pub(crate) const REORDER_SLIDE_MS: u64 = 140;
const CHIP_GAP: f32 = 6.;
/// Chip minimum widths (UI-TAB-OVERFLOW-05): the natural minimum yields to
/// the compression floor before any tab is hidden.
pub(crate) const CHIP_MIN_NATURAL: f32 = 100.;
pub(crate) const CHIP_MIN_FLOOR: f32 = 72.;
/// Width reserved for each overflow affordance chip.
pub(crate) const OVERFLOW_CHIP_W: f32 = 40.;

/// Maps the insertion slot returned after lifting the source out of the list
/// back to the original target slot. Drag intent must carry the stable TabId,
/// but this pure mapping keeps preview/reorder geometry from aliasing B→C as
/// source==target when the source appeared first.
pub(crate) fn target_after_removal(source: usize, insertion: usize, len: usize) -> Option<usize> {
    if source >= len || insertion >= len.saturating_sub(1) {
        return None;
    }
    let target = if insertion >= source {
        insertion + 1
    } else {
        insertion
    };
    (target < len && target != source).then_some(target)
}

/// Builds the adjacent order for a body drag released over a concrete target
/// chip. The target index is from the original list; removing the source first
/// and inserting at that original slot yields the expected order in either
/// direction (B→C = A,C,B; C→A = C,A,B).
pub(crate) fn order_for_hover(source: usize, target: usize, len: usize) -> Option<Vec<usize>> {
    if source >= len || target >= len || source == target {
        return None;
    }
    let mut order: Vec<usize> = (0..len).collect();
    let dragged = order.remove(source);
    order.insert(target.min(order.len()), dragged);
    Some(order)
}

/// The rendered chip window when tabs exceed strip capacity
/// (UI-TAB-OVERFLOW-05). The window always contains the active tab and
/// exposes hidden-tab counts on both edges.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct TabChipWindow {
    pub start: usize,
    pub visible: usize,
    pub chip_min_w: f32,
    pub leading_hidden: usize,
    pub trailing_hidden: usize,
}

/// One pure capacity mapping: compress chips from the natural minimum to the
/// floor, then page with leading/trailing overflow affordances while keeping
/// the active tab inside the rendered window.
pub(crate) fn tab_chip_window(avail: f32, count: usize, active: usize) -> TabChipWindow {
    let fits = |min_w: f32, n: usize, reserve: f32| {
        n as f32 * min_w + n.saturating_sub(1) as f32 * CHIP_GAP + reserve <= avail
    };
    if count == 0 || fits(CHIP_MIN_NATURAL, count, 0.) {
        return TabChipWindow {
            start: 0,
            visible: count,
            chip_min_w: CHIP_MIN_NATURAL,
            leading_hidden: 0,
            trailing_hidden: 0,
        };
    }
    if fits(CHIP_MIN_FLOOR, count, 0.) {
        return TabChipWindow {
            start: 0,
            visible: count,
            chip_min_w: CHIP_MIN_FLOOR,
            leading_hidden: 0,
            trailing_hidden: 0,
        };
    }
    let active = active.min(count - 1);
    let mut visible = count;
    loop {
        let start = if active >= visible {
            (active + 1 - visible).min(count - visible)
        } else {
            0
        };
        let leading = start > 0;
        let trailing = start + visible < count;
        let reserve = (leading as usize + trailing as usize) as f32 * (OVERFLOW_CHIP_W + CHIP_GAP);
        let capacity =
            (((avail - reserve + CHIP_GAP) / (CHIP_MIN_FLOOR + CHIP_GAP)).floor() as usize).max(1);
        if capacity >= visible || visible == 1 {
            return TabChipWindow {
                start,
                visible,
                chip_min_w: CHIP_MIN_FLOOR,
                leading_hidden: start,
                trailing_hidden: count - start - visible,
            };
        }
        visible -= 1;
    }
}

pub(crate) const GRAB_HANDLE_W: f32 = 80.;

const KEEP_SEGMENTS: usize = 3;

pub(crate) fn abbreviate_home(path: &str) -> std::borrow::Cow<'_, str> {
    use std::borrow::Cow;
    if path.starts_with('~') {
        return Cow::Borrowed(path);
    }
    let Some(home) = std::env::var_os("HOME") else {
        return Cow::Borrowed(path);
    };
    let home = home.to_string_lossy();
    let home = home.trim_end_matches('/');
    if home.is_empty() {
        return Cow::Borrowed(path);
    }
    if path == home {
        return Cow::Owned("~".to_string());
    }
    match path.strip_prefix(home) {
        Some(rest) if rest.starts_with('/') => Cow::Owned(format!("~{rest}")),
        _ => Cow::Borrowed(path),
    }
}

fn short_title(raw: &str) -> String {
    let raw = raw.trim();
    if raw.is_empty() {
        return String::new();
    }
    let after_host = match raw.split_once(':') {
        Some((head, tail)) if head.contains('@') => tail,
        _ => raw,
    };
    let after_host = after_host.trim();
    if after_host.is_empty() {
        return String::new();
    }
    let abbreviated = abbreviate_home(after_host);
    let path: &str = abbreviated.as_ref();

    enum Kind {
        Home,
        Absolute,
        Relative,
    }
    let (kind, body) = if let Some(rest) = path.strip_prefix("~/") {
        (Kind::Home, rest)
    } else if path == "~" {
        return "~".to_string();
    } else if let Some(rest) = path.strip_prefix('/') {
        (Kind::Absolute, rest)
    } else {
        (Kind::Relative, path)
    };

    let segments: Vec<&str> = body.split('/').filter(|s| !s.is_empty()).collect();
    if segments.is_empty() {
        return match kind {
            Kind::Home => "~",
            Kind::Absolute => "/",
            Kind::Relative => "",
        }
        .to_string();
    }

    let depth = segments.len() + usize::from(matches!(kind, Kind::Home));
    let mut label = if depth > KEEP_SEGMENTS {
        let tail = &segments[segments.len() - KEEP_SEGMENTS..];
        format!("…/{}", tail.join("/"))
    } else {
        match kind {
            Kind::Home => format!("~/{}", segments.join("/")),
            Kind::Absolute => format!("/{}", segments.join("/")),
            Kind::Relative => segments.join("/"),
        }
    };
    if label.chars().count() > 40 {
        label = format!("{}…", label.chars().take(40).collect::<String>());
    }
    label
}

#[derive(Clone)]
pub(crate) struct DragTabIcon;

impl Render for DragTabIcon {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

#[derive(Clone)]
pub(crate) struct DragTabBody;

impl Render for DragTabBody {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

/// Environment Indicator machine-kind glyph (ENV-INDICATOR-GLYPH-13). The
/// glyph derives from the window Environment authority, never from focus or
/// transport state; connection health stays on the dot and tooltip.
fn environment_indicator_icon(is_remote: bool) -> &'static str {
    if is_remote {
        "icons/machine-remote.svg"
    } else {
        "icons/machine-local.svg"
    }
}

/// Active-tab hierarchy mapping (UI-TAB-HIERARCHY-02). The active tab is an
/// elevated surface with a bottom accent indicator; inactive tabs stay flat.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TabChipHierarchy {
    pub(crate) elevated: bool,
    pub(crate) indicator: bool,
}

fn tab_chip_hierarchy(is_active: bool) -> TabChipHierarchy {
    TabChipHierarchy {
        elevated: is_active,
        indicator: is_active,
    }
}

pub(crate) fn chrome_tile_variant(cx: &gpui::App) -> ButtonCustomVariant {
    chrome_tile_variant_for(false, cx)
}

pub(crate) fn chrome_tile_variant_for(selected: bool, cx: &gpui::App) -> ButtonCustomVariant {
    ButtonCustomVariant::new(cx)
        .color(cx.theme().transparent)
        .foreground(if selected {
            cx.theme().foreground
        } else {
            cx.theme().sidebar_foreground
        })
        .hover(cx.theme().sidebar_accent)
        .active(cx.theme().sidebar_accent)
}

pub(crate) const BUTTON_ICON_SCALE: f32 = 0.75;

pub(crate) fn chrome_tile(button: Button, selected: bool, cx: &gpui::App) -> Button {
    chrome_tile_sized(button, TILE_SIZE, TILE_GLYPH, selected, cx)
}

pub(crate) fn chrome_tile_sized(
    button: Button,
    tile: f32,
    glyph: f32,
    selected: bool,
    cx: &gpui::App,
) -> Button {
    button
        .custom(chrome_tile_variant_for(selected, cx))
        .selected(selected)
        .with_size(px(glyph / BUTTON_ICON_SCALE))
        .w(px(tile))
        .h(px(tile))
}

pub(crate) const LIVE_DOT: u32 = 0x22C55E;

pub(crate) const UNKNOWN_DOT: u32 = 0x9AA0A6;

pub(crate) fn workspace_avatar(
    name: &str,
    live: crate::terminal::pane_liveness::Liveness,
    current: bool,
    size: f32,
    cx: &App,
) -> impl IntoElement + use<> {
    use crate::terminal::pane_liveness::Liveness;
    let dot = match live {
        Liveness::Alive => Some(LIVE_DOT),
        Liveness::Unknown => Some(UNKNOWN_DOT),
        Liveness::Stopped => None,
    };
    let initial: String = name
        .chars()
        .next()
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_else(|| "~".to_string());
    div()
        .relative()
        .flex_shrink_0()
        .size(px(size))
        .child(
            div()
                .size(px(size))
                .rounded_full()
                .bg(cx.theme().secondary)
                .flex()
                .items_center()
                .justify_center()
                .text_size(px((size * 0.46).round()))
                .font_weight(FontWeight::MEDIUM)
                .text_color(cx.theme().foreground.opacity(0.65))
                .child(initial)
                .when(!current, |disc| disc.opacity(0.55)),
        )
        .children(dot.map(|rgb| AgenttyApp::status_dot(rgb, 0, size, cx.theme().popover)))
}

pub(crate) fn select_workspace_action(index: usize) -> Option<Box<dyn gpui::Action>> {
    Some(match index {
        0 => Box::new(SelectWorkspace1) as Box<dyn gpui::Action>,
        1 => Box::new(SelectWorkspace2),
        2 => Box::new(SelectWorkspace3),
        3 => Box::new(SelectWorkspace4),
        4 => Box::new(SelectWorkspace5),
        5 => Box::new(SelectWorkspace6),
        6 => Box::new(SelectWorkspace7),
        7 => Box::new(SelectWorkspace8),
        8 => Box::new(SelectWorkspace9),
        _ => return None,
    })
}

/// Where each title-bar chrome action is anchored (UI-TITLEBAR-CHROME-06).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TitlebarChromeAnchor {
    ContentLeading,
    ContentTrailing,
    WindowChrome,
}

/// Pure placement map for the split Settings / Command Palette chrome.
pub(crate) fn titlebar_chrome_anchor(action: &str) -> Option<TitlebarChromeAnchor> {
    match action {
        "environment" => Some(TitlebarChromeAnchor::ContentLeading),
        "command_palette" => Some(TitlebarChromeAnchor::ContentTrailing),
        "settings" => Some(TitlebarChromeAnchor::WindowChrome),
        _ => None,
    }
}

impl AgenttyApp {
    /// Command Palette icon on the content title bar trailing edge
    /// (UI-TITLEBAR-CHROME-06), opposite the Environment Indicator.
    pub(crate) fn command_palette_tile(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        div().occlude().flex_shrink_0().child(
            chrome_tile(
                Button::new("titlebar-command-palette").icon(IconName::Search),
                false,
                cx,
            )
            .rounded_lg()
            .tooltip(crate::core::i18n::current(cx, "home.shortcut.palette"))
            .on_click(cx.listener(|this, _, window, cx| {
                this.toggle_palette(window, cx);
            })),
        )
    }

    /// Direct Settings icon in window chrome — replaces the ellipsis nest
    /// (UI-TITLEBAR-CHROME-06).
    pub(crate) fn settings_tile(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        div().occlude().flex_shrink_0().child(
            chrome_tile(
                Button::new("titlebar-settings").icon(IconName::Settings2),
                false,
                cx,
            )
            .rounded_lg()
            .tooltip(crate::core::i18n::current(cx, "home.shortcut.settings"))
            .on_click(cx.listener(|this, _, window, cx| {
                this.toggle_settings(window, cx);
            })),
        )
    }

    fn environment_indicator_state(
        remote: Option<&crate::core::session::RemoteRef>,
        remote_label: Option<&str>,
        status: Option<&crate::ui::remote_workspace::RemoteStatus>,
        locale: crate::core::i18n::Locale,
    ) -> (String, String, u32, &'static str) {
        use crate::core::i18n::{tr, trf};
        match remote {
            None => (
                tr(locale, "environment.local.label").into(),
                tr(locale, "environment.local.detail").into(),
                0x22C55E,
                environment_indicator_icon(false),
            ),
            Some(remote) => {
                let state = match status {
                    Some(crate::ui::remote_workspace::RemoteStatus::Attached) => {
                        tr(locale, "environment.ssh").into()
                    }
                    Some(crate::ui::remote_workspace::RemoteStatus::Connecting)
                    | Some(crate::ui::remote_workspace::RemoteStatus::Reconnecting { .. }) => {
                        tr(locale, "environment.connecting").into()
                    }
                    Some(crate::ui::remote_workspace::RemoteStatus::Failed(error)) => {
                        trf(locale, "environment.auth_error", &[("error", error)])
                    }
                    Some(crate::ui::remote_workspace::RemoteStatus::Preempted { .. })
                    | Some(crate::ui::remote_workspace::RemoteStatus::Disconnected)
                    | None => tr(locale, "environment.disconnected").into(),
                };
                let color = match status {
                    Some(crate::ui::remote_workspace::RemoteStatus::Attached) => 0x22C55E,
                    Some(crate::ui::remote_workspace::RemoteStatus::Connecting)
                    | Some(crate::ui::remote_workspace::RemoteStatus::Reconnecting { .. }) => {
                        0xEAB308
                    }
                    Some(crate::ui::remote_workspace::RemoteStatus::Failed(_)) => 0xEF4444,
                    _ => UNKNOWN_DOT,
                };
                (
                    remote_label
                        .map(str::to_owned)
                        .unwrap_or_else(|| remote.target.to_string()),
                    state,
                    color,
                    environment_indicator_icon(true),
                )
            }
        }
    }

    fn environment_indicator_label(&self, cx: &App) -> (String, String, u32, &'static str) {
        let remote = crate::core::session::WorkspaceStore::remote_ref(cx, self.workspace);
        let status = remote.as_ref().and_then(|_| self.remote_status(cx));
        let label = remote
            .as_ref()
            .map(|remote| crate::ui::remote_connect::label_for(&remote.target, cx));
        Self::environment_indicator_state(
            remote.as_ref(),
            label.as_deref(),
            status.as_ref(),
            cx.global::<crate::core::config::Config>().locale.resolve(),
        )
    }

    /// Stable production selector for an environment menu row. The selector
    /// is an observability/accessibility hook only; the menu action still
    /// routes through the canonical open-or-focus primitive below.
    fn environment_menu_target_selector(target: &crate::core::session::RemoteTarget) -> String {
        match target {
            crate::core::session::RemoteTarget::Profile { id } => {
                format!("ENVIRONMENT_MENU_TARGET_PROFILE_{id}")
            }
            _ => format!("ENVIRONMENT_MENU_TARGET_{}", target.connection_key()),
        }
    }

    pub(crate) fn environment_menu(
        mut menu: PopupMenu,
        current_environment: agentty_core::core::environment::EnvironmentId,
        hosts: &[crate::ui::remote_connect::HostChoice],
        app: &gpui::WeakEntity<Self>,
        cx: &App,
    ) -> PopupMenu {
        menu = menu.min_w(px(260.)).item(
            PopupMenuItem::new(crate::core::i18n::current(cx, "menu.this_mac"))
                .disabled(current_environment.is_local())
                .on_click(|_, _window, cx| {
                    crate::ui::windows::open_or_focus_environment(cx, None, None);
                }),
        );
        let mut seen = std::collections::HashSet::new();
        for host in hosts {
            let target = host.target.clone();
            let environment = agentty_core::core::environment::EnvironmentId::for_remote(&target);
            if !seen.insert(environment.clone()) {
                continue;
            }
            let selected = environment == current_environment;
            let label = if host.detail.trim().is_empty() {
                host.label.clone()
            } else {
                format!("{}  ·  {}", host.label, host.detail)
            };
            let selector = Self::environment_menu_target_selector(&target);
            let item = PopupMenuItem::element({
                let selector = selector.clone();
                let label = label.clone();
                move |_window, _cx| {
                    let selector = selector.clone();
                    div()
                        .w_full()
                        .debug_selector(move || selector.clone())
                        .child(label.clone())
                }
            });
            menu = menu.item(item.disabled(selected).on_click({
                let target = target.clone();
                move |_, _window, cx| {
                    crate::ui::windows::open_or_focus_environment(cx, Some(target.clone()), None);
                }
            }));
        }
        menu.separator().item(
            PopupMenuItem::new(crate::core::i18n::current(cx, "menu.manage_ssh_envs")).on_click({
                let app = app.clone();
                move |_, window, cx| {
                    if let Some(app) = app.upgrade() {
                        app.update(cx, |this, cx| {
                            this.open_settings_section(
                                crate::ui::settings::SettingsSection::Ssh,
                                window,
                                cx,
                            );
                        });
                    }
                }
            }),
        )
    }

    pub(crate) fn environment_indicator(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let hosts = crate::ui::remote_connect::available_hosts(cx);
        let (label, state, dot, icon) = self.environment_indicator_label(cx);
        let remote = crate::core::session::WorkspaceStore::remote_ref(cx, self.workspace);
        let detail = remote
            .as_ref()
            .map(|remote| crate::ui::remote_connect::detail_for_target(&remote.target, &hosts))
            .unwrap_or_else(|| {
                crate::core::i18n::current(cx, "environment.local.detail").to_string()
            });
        let current_environment =
            crate::core::session::WorkspaceStore::environment_id(cx, self.workspace);
        let app_for_menu = cx.entity().downgrade();
        div()
            .debug_selector(|| "ENVIRONMENT_MENU_TRIGGER".into())
            .child(
                Button::new("tabstrip-environment-indicator")
                    .child(
                        h_flex()
                            .items_center()
                            .gap_1()
                            .child(
                                gpui::svg()
                                    .path(icon)
                                    .flex_shrink_0()
                                    .size(px(13.))
                                    .text_color(cx.theme().muted_foreground),
                            )
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .size(px(7.))
                                    .rounded_full()
                                    .bg(gpui::rgb(dot)),
                            )
                            .child(div().max_w(px(140.)).truncate().text_xs().child(label))
                            .child(
                                Icon::new(IconName::ChevronDown)
                                    .size(px(10.))
                                    .text_color(cx.theme().muted_foreground),
                            ),
                    )
                    .custom(chrome_tile_variant(cx))
                    .rounded_lg()
                    .tooltip(crate::core::i18n::current_format(
                        cx,
                        "environment.tooltip",
                        &[("state", &state), ("detail", &detail)],
                    ))
                    .dropdown_menu(move |menu, _window, cx| {
                        Self::environment_menu(
                            menu,
                            current_environment.clone(),
                            &hosts,
                            &app_for_menu,
                            cx,
                        )
                    }),
            )
    }

    pub(crate) fn window_chrome(
        &self,
        _window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let panel_open = self.right_panel_open(cx);
        h_flex()
            .flex_shrink_0()
            .items_center()
            .gap(px(2.))
            .pr(px(crate::ui::app::panel_split_chrome_inset()))
            .when(!cfg!(target_os = "macos"), |this| this.pr_1())
            .child(
                div().occlude().flex_shrink_0().child(
                    chrome_tile(
                        Button::new("titlebar-right-panel")
                            .icon(Icon::empty().path("icons/panel-right.svg")),
                        false,
                        cx,
                    )
                    .rounded_lg()
                    .tooltip(if panel_open {
                        crate::core::i18n::current(cx, "panel.hide_detail")
                    } else {
                        crate::core::i18n::current(cx, "panel.show_detail")
                    })
                    .on_click(cx.listener(|this, _, _window, cx| {
                        this.toggle_right_panel(cx);
                    })),
                ),
            )
            .child(self.settings_tile(cx))
    }

    pub(crate) fn right_panel_tabs(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let active_tab = self.right_panel_tab;
        let changed = match &self.right_panel.diff {
            Some(Some(snap)) => {
                let n = snap.files.len() + snap.untracked_count();
                (n > 0).then_some(n)
            }
            _ => None,
        };
        [
            (
                RightPanelTab::Info,
                Icon::empty().path("icons/info.svg"),
                crate::core::i18n::current(cx, "panel.tab.info"),
            ),
            (
                RightPanelTab::Outline,
                Icon::empty().path("icons/list.svg"),
                crate::core::i18n::current(cx, "panel.tab.outline"),
            ),
            (
                RightPanelTab::Changes,
                Icon::empty().path("icons/git-branch.svg"),
                crate::core::i18n::current(cx, "panel.tab.changes"),
            ),
            (
                RightPanelTab::Files,
                Icon::new(IconName::FolderClosed),
                crate::core::i18n::current(cx, "panel.tab.files"),
            ),
            (
                RightPanelTab::Activity,
                Icon::new(IconName::Bot),
                crate::core::i18n::current(cx, "panel.tab.activity"),
            ),
        ]
        .into_iter()
        .map(|(tab, icon, label)| {
            div()
                .occlude()
                .flex_shrink_0()
                .child(
                    chrome_tile(
                        Button::new(("right-panel-tab", tab as usize)).icon(icon),
                        active_tab == tab,
                        cx,
                    )
                    .rounded_lg()
                    .tooltip(match (tab, changed) {
                        (RightPanelTab::Changes, Some(n)) => {
                            SharedString::from(format!("{label} · {n}"))
                        }
                        _ => SharedString::from(label),
                    })
                    .on_click(cx.listener(move |this, _, _window, cx| {
                        this.set_right_panel_tab(tab, cx);
                    })),
                )
                .into_any_element()
        })
        .collect()
    }

    fn status_dot(rgb: u32, unread: usize, size: f32, ring: gpui::Hsla) -> gpui::AnyElement {
        let d = (size * 0.42).max(7.);
        let bg = ring;
        if unread > 0 {
            let nd = (size * 0.72).max(13.0);
            let label = unread.min(9).to_string();
            div()
                .absolute()
                .right(px(-(nd - d) / 2.0 - d * 0.22))
                .bottom(px(-(nd - d) / 2.0 - d * 0.22))
                .size(px(nd))
                .rounded_full()
                .border_1()
                .border_color(bg)
                .bg(gpui::rgb(rgb))
                .flex()
                .items_center()
                .justify_center()
                .text_size(px((nd * 0.62).round()))
                .font_weight(FontWeight::BOLD)
                .text_color(gpui::white())
                .child(label)
                .into_any_element()
        } else {
            div()
                .absolute()
                .right(px(-(d * 0.22)))
                .bottom(px(-(d * 0.22)))
                .size(px(d))
                .rounded_full()
                .border_2()
                .border_color(bg)
                .bg(gpui::rgb(rgb))
                .into_any_element()
        }
    }

    pub(crate) fn tab_avatar(
        &self,
        agent: Option<crate::core::cli_agent::CLIAgent>,
        status: Option<crate::core::cli_agent::AgentStatus>,
        unread: usize,
        ssh: Option<u32>,
        size: f32,
        cx: &App,
    ) -> gpui::AnyElement {
        let base = div()
            .flex_shrink_0()
            .size(px(size))
            .flex()
            .items_center()
            .justify_center();
        match agent {
            Some(agent) => {
                let dot = status
                    .and_then(|s| s.dot_rgb())
                    .map(|rgb| Self::status_dot(rgb, unread, size, cx.theme().background));
                let radius = (size * 0.28).clamp(4.0, 8.0);
                base.relative()
                    .child(crate::ui::agent_icon::agent_icon_badge(
                        agent.icon_path(),
                        size,
                        radius,
                        agent.accent_rgb(),
                        agent.glyph_rgb(),
                        size * 0.54,
                        cx,
                    ))
                    .when_some(dot, |b, dot| b.child(dot))
                    .into_any_element()
            }
            None => {
                let radius = crate::ui::panel_chrome::unbranded_avatar_radius(size);
                base.relative()
                    .rounded(px(radius))
                    .bg(cx.theme().muted)
                    .child(
                        gpui::svg()
                            .path("icons/terminal.svg")
                            .size(px(size * 0.56))
                            .text_color(cx.theme().foreground.opacity(0.65)),
                    )
                    .when_some(ssh, |b, rgb| {
                        b.child(Self::status_dot(rgb, 0, size, cx.theme().background))
                    })
                    .into_any_element()
            }
        }
    }

    pub(crate) fn tab_label(
        &self,
        tab: &Tab,
        index: usize,
        window: Option<&Window>,
        cx: &App,
    ) -> String {
        if let Some(name) = tab.name.as_ref() {
            let trimmed = name.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
        let raw = tab.leaf_title(window, cx);
        let label = short_title(&raw);
        if label.trim().is_empty() {
            crate::core::i18n::current_format(
                cx,
                "tab.shell_default",
                &[("index", &(index + 1).to_string())],
            )
        } else {
            label
        }
    }

    pub(crate) fn attach_new_tab_menu(
        &self,
        button: Button,
        cx: &Context<Self>,
    ) -> impl IntoElement + use<> {
        let shells = self.shells.shells.clone();
        let default_name = self.default_shell_label(cx);
        let app = cx.entity().downgrade();
        button.dropdown_menu(move |menu, _window, _cx| {
            let mut menu = menu.min_w(px(220.));
            for shell in &shells {
                let spec = ShellSpec {
                    program: shell.program.clone(),
                    args: shell.args.clone(),
                    args_are_agentty_defaults: true,
                };
                let open = app.clone();
                let item = if shell.label == default_name {
                    let label: SharedString = shell.label.clone().into();
                    PopupMenuItem::element(move |_window, cx| {
                        h_flex()
                            .w_full()
                            .items_center()
                            .justify_between()
                            .gap_3()
                            .child(label.clone())
                            .child(
                                div()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(crate::core::i18n::current(cx, "common.default")),
                            )
                    })
                } else {
                    PopupMenuItem::new(shell.label.clone())
                };
                menu = menu.item(item.on_click(move |_, window, cx| {
                    if let Some(app) = open.upgrade() {
                        app.update(cx, |this, cx| {
                            this.new_tab_with_shell(Some(spec.clone()), window, cx);
                        });
                    }
                }));
            }
            if shells.is_empty() {
                let open_default = app.clone();
                menu = menu.item(
                    PopupMenuItem::new(crate::core::i18n::current(_cx, "menu.new_tab")).on_click(
                        move |_, window, cx| {
                            if let Some(app) = open_default.upgrade() {
                                app.update(cx, |this, cx| this.new_tab(window, cx));
                            }
                        },
                    ),
                );
            }
            menu
        })
    }

    pub(crate) fn tab_context_menu(
        menu: PopupMenu,
        index: usize,
        below_wording: bool,
        app: &gpui::WeakEntity<Self>,
        window: &Window,
        cx: &App,
    ) -> PopupMenu {
        let Some(entity) = app.upgrade() else {
            return menu;
        };
        let this = entity.read(cx);
        let tab_count = this.tabs.len();
        let cwd = this.tab_cwd(index, window, cx);
        let has_cwd = cwd.is_some();
        let mut menu = menu.min_w(px(200.));

        menu = menu.item(
            PopupMenuItem::new(crate::core::i18n::current(cx, "menu.rename_tab")).on_click({
                let app = app.clone();
                move |_, window, cx| {
                    let _ = app.update(cx, |this, cx| this.start_rename(index, window, cx));
                }
            }),
        );

        let tab = this.tabs.get(index);
        if tab.is_some_and(|t| t.agent(cx).is_some()) {
            let done = tab.and_then(|t| t.agent_status(cx))
                == Some(crate::core::cli_agent::AgentStatus::Done);
            menu = menu.item(
                PopupMenuItem::new(crate::core::i18n::current(cx, "menu.mark_unread"))
                    .disabled(!done)
                    .on_click({
                        let app = app.clone();
                        move |_, _window, cx| {
                            let _ = app.update(cx, |this, cx| this.mark_tab_unread(index, cx));
                        }
                    }),
            );
        }

        let in_repo = this.tab_is_in_repo(index, window, cx);
        if in_repo {
            menu = menu.separator().item(
                PopupMenuItem::new(crate::core::i18n::current(cx, "menu.new_worktree_tab"))
                    .on_click({
                        let app = app.clone();
                        move |_, window, cx| {
                            let _ =
                                app.update(cx, |this, cx| this.new_worktree_tab(index, window, cx));
                        }
                    }),
            );
        }

        let agent_session = this.tab_agent_session(index, window, cx);
        if let Some((source, session)) = &agent_session
            && let Some(label) = session.fork_label
        {
            if !in_repo {
                menu = menu.separator();
            }
            let forkable = session.forkable();
            menu = menu.item(PopupMenuItem::new(label).disabled(!forkable).on_click({
                let app = app.clone();
                let source = source.clone();
                move |_, window, cx| {
                    let source = source.clone();
                    let _ = app.update(cx, |this, cx| {
                        this.fork_agent_session(
                            index,
                            source,
                            crate::ui::app::ForkPlacement::NewTab,
                            window,
                            cx,
                        )
                    });
                }
            }));
        }

        menu = menu
            .separator()
            .item(
                PopupMenuItem::new(crate::core::i18n::current(cx, "menu.split_right")).on_click({
                    let app = app.clone();
                    move |_, window, cx| {
                        let _ = app.update(cx, |this, cx| {
                            this.activate(index, window, cx);
                            this.split(Axis::Horizontal, window, cx);
                        });
                    }
                }),
            )
            .item(
                PopupMenuItem::new(crate::core::i18n::current(cx, "menu.split_down")).on_click({
                    let app = app.clone();
                    move |_, window, cx| {
                        let _ = app.update(cx, |this, cx| {
                            this.activate(index, window, cx);
                            this.split(Axis::Vertical, window, cx);
                        });
                    }
                }),
            )
            .item(
                PopupMenuItem::new(crate::core::i18n::current(cx, "menu.move_to_new_window"))
                    .on_click({
                        let app = app.clone();
                        move |_, window, cx| {
                            let _ = app.update(cx, |this, cx| {
                                this.move_tab_to_new_window(index, window, cx);
                            });
                        }
                    }),
            );

        menu = menu.separator().item(
            PopupMenuItem::new(crate::core::i18n::current(cx, "menu.copy_cwd"))
                .disabled(!has_cwd)
                .on_click(move |_, _window, cx| {
                    if let Some(cwd) = cwd.as_ref() {
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                            cwd.display().to_string(),
                        ));
                    }
                }),
        );

        if let Some(session_id) = agent_session.map(|(_, s)| s.session_id) {
            menu = menu.item(
                PopupMenuItem::new(crate::core::i18n::current(cx, "menu.copy_session_id"))
                    .disabled(session_id.is_none())
                    .on_click(move |_, _window, cx| {
                        if let Some(id) = session_id.as_ref() {
                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(id.clone()));
                        }
                    }),
            );
        }

        menu.separator()
            .item(
                PopupMenuItem::new(crate::core::i18n::current(cx, "menu.close_tab")).on_click({
                    let app = app.clone();
                    move |_, window, cx| {
                        let _ = app.update(cx, |this, cx| this.close_tab(index, window, cx));
                    }
                }),
            )
            .item(
                PopupMenuItem::new(crate::core::i18n::current(cx, "menu.close_other_tabs"))
                    .disabled(tab_count <= 1)
                    .on_click({
                        let app = app.clone();
                        move |_, window, cx| {
                            let _ =
                                app.update(cx, |this, cx| this.close_other_tabs(index, window, cx));
                        }
                    }),
            )
            .item(
                PopupMenuItem::new(if below_wording {
                    crate::core::i18n::current(cx, "menu.close_tabs_below")
                } else {
                    crate::core::i18n::current(cx, "menu.close_tabs_right")
                })
                .disabled(index + 1 >= tab_count)
                .on_click({
                    let app = app.clone();
                    move |_, window, cx| {
                        let _ =
                            app.update(cx, |this, cx| this.close_tabs_right_of(index, window, cx));
                    }
                }),
            )
    }

    pub(crate) fn tab_strip(
        &self,
        show_chips: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let active = self.active;
        let show_badges = self.mod_hint_badges;
        let strip_w = if cfg!(target_os = "macos") {
            (window.viewport_size().width - px(80.)).max(px(160.))
        } else {
            (window.viewport_size().width - px(114.)).max(px(140.))
        };
        let chrome_band_w = (!cfg!(target_os = "macos") && self.right_panel_open(cx)).then(|| {
            (self.right_panel_px(window, cx) - crate::ui::app::WINDOW_CONTROLS_W - 1.).max(0.)
        });
        // Content trailing always hosts the Command Palette tile
        // (UI-TITLEBAR-CHROME-06); window chrome may add panel + Settings.
        let palette_w = crate::ui::app::TILE_SIZE + 2.;
        let chrome_on_strip = !self.right_panel_open(cx) || !cfg!(target_os = "macos");
        let corner_w = chrome_band_w.unwrap_or_else(|| {
            let trailing_pad = if cfg!(target_os = "macos") {
                tile_trailing_inset()
            } else {
                4.
            };
            if chrome_on_strip {
                trailing_pad + crate::ui::app::TILE_SIZE + 2. + crate::ui::app::TILE_SIZE
            } else {
                trailing_pad
            }
        });
        let fixed_w = 3. * CHIP_GAP + crate::ui::app::TILE_SIZE + palette_w + corner_w;
        let chips_avail = (strip_w - px(fixed_w + GRAB_HANDLE_W)).max(px(80.));
        let mut chips = h_flex()
            .items_center()
            .gap(px(CHIP_GAP))
            .min_w_0()
            .max_w(chips_avail)
            .overflow_hidden();
        let chip_window = tab_chip_window(f32::from(chips_avail), self.tabs.len(), active);

        let slots: Rc<RefCell<Vec<Bounds<Pixels>>>> =
            Rc::new(RefCell::new(vec![Bounds::default(); self.tabs.len()]));
        let preview = reorder::preview(
            &self.reorder,
            &Surface::Strip,
            self.tabs.len(),
            window.mouse_position(),
        );
        let display: Vec<usize> = match &preview {
            Some(p) => {
                // A Merge is valid only while the pointer is over a concrete
                // target chip.  Geometry can still produce an insertion slot
                // in a gap (or outside the strip), but that slot must never
                // synthesize a merge target.  Convert the hovered original
                // index through the one removal-mapping helper so source
                // before/after target cannot alias.
                let target_tab = p.hovered.and_then(|hovered| {
                    let insertion = if hovered >= p.from {
                        hovered.saturating_sub(1)
                    } else {
                        hovered
                    };
                    target_after_removal(p.from, insertion, self.tabs.len())
                        .filter(|mapped| *mapped == hovered)
                        .and_then(|index| self.tabs.get(index))
                        .map(|tab| tab.tree_id.get())
                });
                let tab_intent = self.reorder.borrow().as_ref().and_then(|r| r.tab_intent);
                let order = match (tab_intent, p.hovered) {
                    (Some(TabDragIntent::Reorder), Some(target)) => {
                        order_for_hover(p.from, target, self.tabs.len())
                            .unwrap_or_else(|| p.order.clone())
                    }
                    _ => p.order.clone(),
                };
                reorder::set_tab_pending(
                    &self.reorder,
                    &Surface::Strip,
                    order.clone(),
                    p.target,
                    target_tab,
                );
                order
            }
            None => (0..self.tabs.len()).collect(),
        };

        let render_all = preview.is_some();
        if !render_all && chip_window.leading_hidden > 0 {
            let target = chip_window.start - 1;
            let hidden = chip_window.leading_hidden;
            chips = chips.child(
                h_flex()
                    .id("tab-overflow-leading")
                    .occlude()
                    .flex_shrink_0()
                    .cursor_pointer()
                    .items_center()
                    .justify_center()
                    .gap_1()
                    .h(px(30.))
                    .min_w(px(OVERFLOW_CHIP_W))
                    .px_2()
                    .rounded_lg()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .hover(|s| s.bg(cx.theme().muted))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.activate(target, window, cx);
                    }))
                    .child(
                        Icon::new(IconName::ChevronLeft)
                            .size(px(10.))
                            .text_color(cx.theme().muted_foreground),
                    )
                    .child(format!("{hidden}")),
            );
        }

        for i in display {
            if !show_chips {
                break;
            }
            if !render_all
                && (i < chip_window.start || i >= chip_window.start + chip_window.visible)
            {
                continue;
            }
            let dragged = preview.as_ref().is_some_and(|p| p.from == i);
            let tab = &self.tabs[i];
            let is_active = i == active;
            let label = self.tab_label(tab, i, Some(window), cx);
            let ssh_dot = self.tab_ssh_dot(tab, cx);
            let agent = tab.agent(cx);
            let agent_status = tab.agent_status(cx);
            let agent_unread = tab.agent_unread_count(cx);

            let rename_input = self
                .renaming
                .as_ref()
                .filter(|r| r.index == i)
                .map(|r| r.input.clone());
            let label_region = match rename_input {
                Some(input) => div()
                    .id(("tab-rename", i))
                    .flex_1()
                    .min_w_0()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(Input::new(&input).appearance(false))
                    .into_any_element(),
                None => div()
                    .id(("tab-label", i))
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .when(is_active, |d| d.font_weight(FontWeight::MEDIUM))
                    .child(label)
                    .into_any_element(),
            };

            let source_tab_id = tab.tree_id.get();
            let icon_region = h_flex()
                .id(("tab-icon-drag", i))
                .debug_selector(move || format!("TAB_CHIP_{i}_ICON"))
                .on_drag(DragTabIcon, {
                    let state = self.reorder.clone();
                    let slots = slots.clone();
                    move |_drag, grab, _window, cx| {
                        cx.stop_propagation();
                        *state.borrow_mut() = Some(Reorder::new_tab(
                            Surface::Strip,
                            i,
                            slots.borrow().clone(),
                            Axis::Horizontal,
                            px(CHIP_GAP),
                            grab,
                            source_tab_id,
                            TabDragIntent::Merge,
                            TabDragHitZone::Icon,
                        ));
                        cx.new(|_| DragTabIcon)
                    }
                })
                .flex_shrink_0()
                .items_center()
                .justify_center()
                .size(px(20.))
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .when_some(ssh_dot, |region, rgb| {
                    region.child(
                        div()
                            .flex_shrink_0()
                            .size(px(6.))
                            .rounded_full()
                            .bg(gpui::rgb(rgb)),
                    )
                })
                .when_some(agent, |region, agent| {
                    region.child(self.tab_avatar(
                        Some(agent),
                        agent_status,
                        agent_unread,
                        None,
                        18.,
                        cx,
                    ))
                })
                .child("⋮");
            let body_region = div()
                .id(("tab-body-drag", i))
                .debug_selector(move || format!("TAB_CHIP_{i}_BODY"))
                .on_drag(DragTabBody, {
                    let state = self.reorder.clone();
                    let slots = slots.clone();
                    move |_drag, grab, _window, cx| {
                        cx.stop_propagation();
                        *state.borrow_mut() = Some(Reorder::new_tab(
                            Surface::Strip,
                            i,
                            slots.borrow().clone(),
                            Axis::Horizontal,
                            px(CHIP_GAP),
                            grab,
                            source_tab_id,
                            TabDragIntent::Reorder,
                            TabDragHitZone::Body,
                        ));
                        cx.new(|_| DragTabBody)
                    }
                })
                .flex_1()
                .min_w_0()
                .child(label_region);

            let chip = h_flex()
                .id(("tab-chip", i))
                .debug_selector(move || format!("TAB_CHIP_{i}"))
                .occlude()
                .group(SharedString::from(format!("tab-chip-{i}")))
                .cursor_pointer()
                .items_center()
                .justify_between()
                .gap_1p5()
                .h(px(30.))
                .min_w(px(chip_window.chip_min_w))
                .flex_shrink(1.)
                .pl_3()
                .pr_1p5()
                .rounded_lg()
                .when(tab_chip_hierarchy(is_active).elevated, |s| {
                    s.bg(cx.theme().secondary)
                        .text_color(cx.theme().foreground)
                        .border_1()
                        .border_color(cx.theme().border)
                })
                .when(!tab_chip_hierarchy(is_active).elevated, |s| {
                    s.text_color(cx.theme().muted_foreground)
                        .hover(|s| s.bg(cx.theme().muted))
                })
                .when(dragged, |s| s.opacity(0.75))
                .child(
                    canvas(
                        {
                            let slots = slots.clone();
                            move |bounds, _window, _cx| {
                                if let Some(slot) = slots.borrow_mut().get_mut(i) {
                                    *slot = bounds;
                                }
                            }
                        },
                        |_, _, _, _| {},
                    )
                    .absolute()
                    .inset_0(),
                )
                .when(tab_chip_hierarchy(is_active).indicator, |chip| {
                    chip.child(
                        div()
                            .absolute()
                            .left(px(10.))
                            .right(px(10.))
                            .bottom(px(1.5))
                            .h(px(2.))
                            .rounded_full()
                            .bg(cx.theme().accent),
                    )
                })
                .on_double_click(|_, window, _| {
                    window.titlebar_double_click();
                })
                .on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.activate(i, window, cx);
                }))
                .child(icon_region)
                .child(body_region)
                .when(show_badges && i < 9, |chip| {
                    chip.child(
                        div()
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(20.))
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(if is_active {
                                cx.theme().foreground
                            } else {
                                cx.theme().muted_foreground
                            })
                            .child(tab_badge_label(i)),
                    )
                })
                .when(!(show_badges && i < 9), |chip| {
                    let backing = if is_active {
                        cx.theme().secondary
                    } else {
                        cx.theme().muted
                    };
                    let mut fade_from = backing;
                    fade_from.a = 0.;
                    chip.child(
                        h_flex()
                            .absolute()
                            .top(px(5.))
                            .right(px(6.))
                            .opacity(0.)
                            .group_hover(SharedString::from(format!("tab-chip-{i}")), |s| {
                                s.opacity(1.)
                            })
                            .child(div().w(px(10.)).h(px(20.)).bg(linear_gradient(
                                90.,
                                linear_color_stop(fade_from, 0.),
                                linear_color_stop(backing, 1.),
                            )))
                            .child(
                                div().bg(backing).child(
                                    Button::new(("tab-close", i))
                                        .icon(IconName::Close)
                                        .ghost()
                                        .xsmall()
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.close_tab(i, window, cx);
                                        })),
                                ),
                            ),
                    )
                });

            let menu_app = cx.entity().downgrade();
            let chip = chip.context_menu(move |menu, window, cx| {
                Self::tab_context_menu(menu, i, false, &menu_app, window, cx)
            });
            chips = chips.child(match &preview {
                Some(p) if p.from == i => deferred(chip.relative().left(p.held)).into_any_element(),
                Some(p) => {
                    let offset = p.offsets[i].as_f32();
                    chip.with_animation(
                        (
                            SharedString::from(format!("chip-slide-{}", p.generation)),
                            i,
                        ),
                        Animation::new(std::time::Duration::from_millis(REORDER_SLIDE_MS))
                            .with_easing(ease_out_quint()),
                        move |el, delta| el.left(px(offset * (1. - delta))),
                    )
                    .into_any_element()
                }
                None => chip.into_any_element(),
            });
        }

        if !render_all && chip_window.trailing_hidden > 0 {
            let target = chip_window.start + chip_window.visible;
            let hidden = chip_window.trailing_hidden;
            chips = chips.child(
                h_flex()
                    .id("tab-overflow-trailing")
                    .occlude()
                    .flex_shrink_0()
                    .cursor_pointer()
                    .items_center()
                    .justify_center()
                    .gap_1()
                    .h(px(30.))
                    .min_w(px(OVERFLOW_CHIP_W))
                    .px_2()
                    .rounded_lg()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .hover(|s| s.bg(cx.theme().muted))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.activate(target, window, cx);
                    }))
                    .child(format!("{hidden}"))
                    .child(
                        Icon::new(IconName::ChevronRight)
                            .size(px(10.))
                            .text_color(cx.theme().muted_foreground),
                    ),
            );
        }

        let make_add = |id: &'static str, this: &Self, cx: &mut Context<Self>| {
            div().occlude().flex_shrink_0().child(
                this.attach_new_tab_menu(
                    chrome_tile_sized(
                        Button::new(id).icon(Icon::new(IconName::Plus)),
                        TILE_SIZE,
                        TILE_GLYPH_LINE,
                        false,
                        cx,
                    )
                    .rounded_lg(),
                    cx,
                ),
            )
        };
        // Chip strip owns Plus when horizontal; left-rail open owns Plus beside
        // the Environment Indicator (UI-TITLEBAR-CHROME-06).
        let strip_add = show_chips.then(|| make_add("tab-add", self, cx));
        let rail_new_tab = (!show_chips && self.left_panel_open(cx))
            .then(|| make_add("titlebar-add-rail", self, cx));

        let rail_collapsed = !show_chips && !self.left_panel_open(cx);
        let left_group = rail_collapsed.then(|| {
            h_flex()
                .flex_shrink_0()
                .items_center()
                .gap(px(2.))
                .ml(px(crate::ui::app::title_bar_hug_offset()))
                .when_some(crate::ui::app::window_mark(), |group, mark| {
                    group.child(
                        div()
                            .flex_shrink_0()
                            .pl(px(crate::ui::app::CONTENT_INSET
                                - crate::ui::app::tile_trailing_inset()))
                            .pr(px(4.))
                            .child(mark),
                    )
                })
                .child(
                    div().occlude().flex_shrink_0().child(
                        self.attach_new_tab_menu(
                            chrome_tile_sized(
                                Button::new("titlebar-add-collapsed")
                                    .icon(Icon::new(IconName::Plus)),
                                TILE_SIZE,
                                TILE_GLYPH_LINE,
                                false,
                                cx,
                            )
                            .rounded_lg(),
                            cx,
                        ),
                    ),
                )
                .child(
                    div().occlude().flex_shrink_0().child(
                        chrome_tile(
                            Button::new("titlebar-expand-sidebar")
                                .icon(Icon::empty().path("icons/panel-left.svg")),
                            false,
                            cx,
                        )
                        .rounded_lg()
                        .tooltip(crate::core::i18n::current(cx, "sidebar.show"))
                        .on_click(cx.listener(|this, _, _window, cx| this.toggle_left_panel(cx))),
                    ),
                )
        });

        let panel_open = self.right_panel_open(cx);
        let right_chrome =
            (!panel_open || !cfg!(target_os = "macos")).then(|| self.window_chrome(window, cx));
        let split_chrome = crate::ui::app::panel_split_chrome_inset();
        let needs_trailing_split_pad = right_chrome.is_none();
        let environment_menu_hosts = crate::ui::remote_connect::available_hosts(cx);
        let environment_menu_current =
            crate::core::session::WorkspaceStore::environment_id(cx, self.workspace);
        let environment_menu_app = cx.entity().downgrade();

        h_flex()
            .id("tab-strip")
            .context_menu(move |menu, _window, cx| {
                Self::environment_menu(
                    menu,
                    environment_menu_current.clone(),
                    &environment_menu_hosts,
                    &environment_menu_app,
                    cx,
                )
            })
            .items_center()
            .gap_1p5()
            .when(show_chips, |this| this.w(strip_w))
            .when(!show_chips, |this| this.w_full())
            .pl_0()
            .min_w_0()
            .child(
                div()
                    .occlude()
                    .flex_shrink_0()
                    .ml(px(crate::ui::app::title_bar_content_lead(
                        self.left_panel_open(cx),
                    )))
                    .child(self.environment_indicator(cx)),
            )
            .when_some(rail_new_tab, |this, add| this.child(add))
            .when_some(left_group, |this, g| this.child(g))
            .child(chips)
            .when_some(strip_add, |this, add| this.child(add))
            .child(div().flex_1().min_w(px(GRAB_HANDLE_W)))
            .child(self.command_palette_tile(cx))
            .when_some(right_chrome, |this, chrome| match chrome_band_w {
                Some(w) => this.child(
                    h_flex()
                        .flex_none()
                        .w(px(w))
                        .items_center()
                        .pl(px(split_chrome))
                        .child(chrome),
                ),
                None => this.child(chrome),
            })
            // When the right panel owns window chrome, the content column still
            // needs the shared split inset behind the trailing palette tile.
            .when(needs_trailing_split_pad, |this| this.pr(px(split_chrome)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_title_strips_user_host_and_shows_shallow_path_in_full() {
        assert_eq!(short_title("user@host:~/projects/app"), "~/projects/app");
        assert_eq!(short_title("/usr/local/bin"), "/usr/local/bin");
        assert_eq!(short_title("plain"), "plain");
    }

    #[test]
    fn short_title_truncates_deep_paths_to_trailing_segments() {
        assert_eq!(
            short_title("user@host:~/repo/025/agentty"),
            "…/repo/025/agentty"
        );
        assert_eq!(short_title("/usr/local/share/man"), "…/local/share/man");
        assert_eq!(short_title("a/b/c/d"), "…/b/c/d");
    }

    #[test]
    fn short_title_keeps_home_tilde_and_normalizes_trailing_slash() {
        assert_eq!(short_title("user@host:~"), "~");
        assert_eq!(short_title("~"), "~");
        assert_eq!(short_title("a/b/c/"), "a/b/c");
    }

    #[test]
    fn environment_indicator_labels_local_and_remote_authority() {
        let (label, state, color, icon) = AgenttyApp::environment_indicator_state(
            None,
            None,
            None,
            crate::core::i18n::Locale::EnUs,
        );
        assert_eq!(
            (label.as_str(), state.as_str(), color, icon),
            (
                "This Mac",
                "Local environment",
                0x22C55E,
                "icons/machine-local.svg"
            )
        );

        let remote = crate::core::session::RemoteRef::new(
            crate::core::session::RemoteTarget::Alias {
                alias: "build".into(),
            },
            crate::core::session::WorkspaceId::new(),
        );
        let (label, state, color, icon) = AgenttyApp::environment_indicator_state(
            Some(&remote),
            Some("build"),
            Some(&crate::ui::remote_workspace::RemoteStatus::Attached),
            crate::core::i18n::Locale::EnUs,
        );
        assert_eq!(label, "build");
        assert_eq!(state, "SSH");
        assert_eq!(color, 0x22C55E);
        assert_eq!(icon, "icons/machine-remote.svg");
    }

    #[test]
    fn profile_environment_uses_profile_name_without_changing_environment_identity() {
        let mut config = crate::core::config::Config::default();
        let mut profile = crate::core::ssh_profile::SshProfile::new("o1");
        profile.host = "161.153.45.244".into();
        let target = crate::core::session::RemoteTarget::Profile { id: profile.id };
        let identity =
            agentty_core::core::environment::EnvironmentId::for_remote(&target).to_string();
        config.ssh_profiles.push(profile);

        assert_eq!(
            crate::ui::remote_connect::label_for_config(&target, &config),
            "o1"
        );
        assert!(identity.starts_with("ssh-profile:"));
        assert!(!identity.contains("o1"));
    }

    #[test]
    fn missing_profile_uses_stable_non_uuid_label() {
        let id = uuid::Uuid::parse_str("13929bd1-48a9-43b8-a211-2adfd707608d").unwrap();
        let target = crate::core::session::RemoteTarget::Profile { id };
        let label = crate::ui::remote_connect::label_for_config(
            &target,
            &crate::core::config::Config::default(),
        );
        assert_eq!(label, "SSH profile 13929bd1");
        assert!(!label.contains(&id.to_string()));
    }

    #[test]
    fn authentication_diagnostic_is_visible_in_environment_indicator() {
        let remote = crate::core::session::RemoteRef::new(
            crate::core::session::RemoteTarget::Alias {
                alias: "build".into(),
            },
            crate::core::session::WorkspaceId::new(),
        );
        let detail = "no default identity files and SSH agent has no identities";
        let (_, state, color, icon) = AgenttyApp::environment_indicator_state(
            Some(&remote),
            Some("build"),
            Some(&crate::ui::remote_workspace::RemoteStatus::Failed(
                detail.into(),
            )),
            crate::core::i18n::Locale::EnUs,
        );
        assert_eq!(icon, "icons/machine-remote.svg");
        assert!(state.contains(detail));
        assert_ne!(state, "Authentication error");
        assert_eq!(color, 0xEF4444);
    }

    #[test]
    fn short_title_blank_input_is_empty_and_long_names_are_clamped() {
        assert_eq!(short_title("   "), "");
        let long = "a".repeat(50);
        let out = short_title(&long);
        assert_eq!(out.chars().count(), 41);
        assert!(out.ends_with('…'));
    }
    #[test]
    fn environment_indicator_distinguishes_local_and_remote_glyphs() {
        assert_eq!(environment_indicator_icon(false), "icons/machine-local.svg");
        assert_eq!(environment_indicator_icon(true), "icons/machine-remote.svg");
        assert_ne!(
            environment_indicator_icon(false),
            environment_indicator_icon(true)
        );
    }

    #[test]
    fn environment_indicator_tooltip_uses_resolved_endpoint_context() {
        let target = crate::core::session::RemoteTarget::Alias {
            alias: "build".into(),
        };
        let hosts = vec![crate::ui::remote_connect::HostChoice {
            target: target.clone(),
            label: "build".into(),
            detail: "deploy@10.0.0.8:2222".into(),
        }];
        assert_eq!(
            crate::ui::remote_connect::detail_for_target(&target, &hosts),
            "deploy@10.0.0.8:2222"
        );
        let missing = crate::core::session::RemoteTarget::Alias {
            alias: "missing".into(),
        };
        assert_eq!(
            crate::ui::remote_connect::detail_for_target(&missing, &hosts),
            "missing"
        );
    }

    #[test]
    fn titlebar_chrome_places_palette_on_content_bar_and_settings_direct() {
        assert_eq!(
            titlebar_chrome_anchor("environment"),
            Some(TitlebarChromeAnchor::ContentLeading)
        );
        assert_eq!(
            titlebar_chrome_anchor("command_palette"),
            Some(TitlebarChromeAnchor::ContentTrailing)
        );
        assert_eq!(
            titlebar_chrome_anchor("settings"),
            Some(TitlebarChromeAnchor::WindowChrome)
        );
        let source = include_str!("tab_strip.rs");
        let prod = source.split("#[cfg(test)]").next().unwrap_or(source);
        assert!(
            prod.contains("titlebar-command-palette") && prod.contains("command_palette_tile(cx)"),
            "Command Palette must be a content title-bar icon"
        );
        assert!(
            prod.contains("titlebar-settings") && prod.contains("settings_tile(cx)"),
            "Settings must be a direct window-chrome icon"
        );
        assert!(
            !prod.contains("titlebar-app-menu") && !prod.contains("IconName::Ellipsis"),
            "ellipsis app menu nesting palette+settings is forbidden"
        );
    }

    #[test]
    fn left_rail_keeps_new_tab_affordance() {
        let source = include_str!("tab_strip.rs");
        let prod = source.split("#[cfg(test)]").next().unwrap_or(source);
        assert!(
            prod.contains("titlebar-add-rail")
                && prod.contains("left_panel_open(cx)")
                && prod.contains("rail_new_tab"),
            "left-rail open must keep New Tab Plus beside the Environment Indicator"
        );
        assert!(
            !prod.contains("\"Close Tabs Below\"") && !prod.contains("\"Close Tabs to the Right\""),
            "tab close-direction menus must use i18n keys"
        );
        assert!(
            prod.contains("menu.close_tabs_below") && prod.contains("menu.close_tabs_right"),
            "close-tabs direction labels must be localized"
        );
    }

    #[test]
    fn content_column_trailing_respects_panel_split_chrome_inset() {
        let source = include_str!("tab_strip.rs");
        let prod = source.split("#[cfg(test)]").next().unwrap_or(source);
        assert!(
            prod.contains("needs_trailing_split_pad")
                && prod.contains("panel_split_chrome_inset()")
                && prod.contains(".when(needs_trailing_split_pad"),
            "when the right panel owns window chrome, the content column must pad the trailing palette with panel_split_chrome_inset"
        );
    }

    #[test]
    fn environment_menu_uses_canonical_open_or_focus_path() {
        let source = include_str!("tab_strip.rs");
        let prod = source.split("#[cfg(test)]").next().unwrap_or(source);
        assert!(
            prod.contains("environment_menu_hosts")
                && prod.contains(".context_menu(move |menu")
                && prod.contains("Self::environment_menu(")
        );
        assert!(
            prod.contains("open_or_focus_environment(cx, Some(target.clone()), None)")
                && prod.contains("EnvironmentId::for_remote")
        );
        let windows = include_str!("windows.rs");
        assert!(
            windows.contains("open_or_focus_environment")
                && windows.contains("WindowRegistry::window_for_environment"),
            "environment window opening must use the canonical deduplicating path"
        );
        let app = include_str!("app.rs");
        assert!(
            app.contains("remote_connect::label_for")
                && app.contains("unwrap_or_else(|| \"Local\""),
            "new environment windows must receive an environment-derived title"
        );
    }

    #[test]
    fn window_session_split_menu_routes_to_canonical_split_primitive() {
        let source = include_str!("tab_strip.rs");
        let prod = source.split("#[cfg(test)]").next().unwrap_or(source);
        assert!(
            prod.contains("menu.split_right") && prod.contains("menu.split_down"),
            "session context menu must expose both canonical split directions"
        );
        assert!(
            prod.contains("this.split(Axis::Horizontal, window, cx)")
                && prod.contains("this.split(Axis::Vertical, window, cx)"),
            "split menu actions must route through AgenttyApp::split"
        );
        assert!(
            !prod.contains("split_leaf") && !prod.contains("PaneNode::Split"),
            "tab UI must not construct a parallel split tree"
        );
        assert!(
            prod.contains("menu.move_to_new_window")
                && prod.contains("this.move_tab_to_new_window(index, window, cx)"),
            "tab context menu should expose and route the move-to-new-window action"
        );
    }

    #[cfg(unix)]
    fn top_tab_strip(visual: &mut gpui::VisualTestContext) {
        visual.update(|_, cx| {
            let mut config = cx.global::<crate::core::config::Config>().clone();
            config.tab_bar_position = crate::core::config::TabBarPosition::Top;
            config.cursor_blink = false;
            cx.set_global(config);
        });
    }

    #[cfg(unix)]
    fn install_quiet_tabs(
        app: &gpui::Entity<AgenttyApp>,
        visual: &mut gpui::VisualTestContext,
        pane_ids: &[u64],
        names: &[&str],
    ) -> Vec<gpui::EntityId> {
        assert_eq!(pane_ids.len(), names.len());
        app.update_in(visual, |app, window, cx| {
            let mut tabs = Vec::new();
            let mut entities = Vec::new();
            for (&pane_id, &name) in pane_ids.iter().zip(names) {
                let (view, _stream) = crate::terminal::view::quiet_test_pane(pane_id, window, cx);
                entities.push(view.entity_id());
                let mut tab = Tab::new(crate::ui::pane::Pane::leaf(
                    crate::ui::pane::PaneSlot::Ready(view),
                ));
                tab.name = Some(name.to_string());
                tabs.push(tab);
            }
            app.tabs = tabs;
            app.active = 0;
            app.focus_active(window, cx);
            cx.notify();
            entities
        })
    }

    #[cfg(unix)]
    fn selector_center(
        visual: &mut gpui::VisualTestContext,
        selector: &'static str,
    ) -> gpui::Point<Pixels> {
        let bounds = visual
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("production selector {selector} must be rendered"));
        gpui::point(
            bounds.origin.x + bounds.size.width / 2.,
            bounds.origin.y + bounds.size.height / 2.,
        )
    }

    #[cfg(unix)]
    fn drag_selectors(
        visual: &mut gpui::VisualTestContext,
        source: &'static str,
        target: &'static str,
    ) {
        let source_point = selector_center(visual, source);
        let target_point = selector_center(visual, target);
        let threshold_point = gpui::point(source_point.x - px(16.), source_point.y);
        visual.simulate_mouse_move(source_point, None, gpui::Modifiers::none());
        visual.simulate_mouse_down(source_point, MouseButton::Left, gpui::Modifiers::none());
        visual.simulate_mouse_move(
            threshold_point,
            Some(MouseButton::Left),
            gpui::Modifiers::none(),
        );
        visual.run_until_parked();
        visual.simulate_mouse_move(
            target_point,
            Some(MouseButton::Left),
            gpui::Modifiers::none(),
        );
        visual.simulate_mouse_up(target_point, MouseButton::Left, gpui::Modifiers::none());
        visual.run_until_parked();
    }

    #[cfg(unix)]
    #[gpui::test]
    fn tab_strip_icon_drag_splits_single_pane_target_and_source(cx: &mut gpui::TestAppContext) {
        let (app, mut visual) = crate::ui::app::test_window::harness(cx);
        top_tab_strip(&mut visual);
        install_quiet_tabs(&app, &mut visual, &[51, 52], &["target", "source"]);
        visual.run_until_parked();

        drag_selectors(&mut visual, "TAB_CHIP_1_ICON", "TAB_CHIP_0_ICON");

        app.update_in(&mut visual, |app, _, _| {
            assert_eq!(
                app.tabs.len(),
                1,
                "a valid 1+1 icon merge removes one top-level tab"
            );
            assert_eq!(app.tabs[0].pane.leaves().len(), 2);
        });
    }

    #[cfg(unix)]
    #[gpui::test]
    fn tab_strip_body_drag_reorders_source_before_target_and_persists(
        cx: &mut gpui::TestAppContext,
    ) {
        let (app, mut visual) = crate::ui::app::test_window::harness(cx);
        top_tab_strip(&mut visual);
        install_quiet_tabs(&app, &mut visual, &[61, 62, 63], &["A", "B", "C"]);
        visual.run_until_parked();

        drag_selectors(&mut visual, "TAB_CHIP_1_BODY", "TAB_CHIP_2_BODY");

        app.update_in(&mut visual, |app, _window, cx| {
            let names = app
                .tabs
                .iter()
                .map(|tab| tab.name.clone().unwrap_or_default())
                .collect::<Vec<_>>();
            assert_eq!(names, vec!["A", "C", "B"]);
            let (desired, _, held) = crate::ui::tree_sync::desired_tabs(app, cx);
            assert!(held.is_empty(), "all terminal tabs must remain persistable");
            assert_eq!(
                desired
                    .iter()
                    .map(|tab| tab.name.clone().unwrap_or_default())
                    .collect::<Vec<_>>(),
                names,
                "reorder commits through the canonical persisted desired order"
            );
        });
    }

    #[cfg(unix)]
    #[gpui::test]
    fn tab_strip_body_drag_reorders_source_after_target_and_persists(
        cx: &mut gpui::TestAppContext,
    ) {
        let (app, mut visual) = crate::ui::app::test_window::harness(cx);
        top_tab_strip(&mut visual);
        install_quiet_tabs(&app, &mut visual, &[71, 72, 73], &["A", "B", "C"]);
        visual.run_until_parked();

        drag_selectors(&mut visual, "TAB_CHIP_2_BODY", "TAB_CHIP_0_BODY");

        app.update_in(&mut visual, |app, _window, cx| {
            let names = app
                .tabs
                .iter()
                .map(|tab| tab.name.clone().unwrap_or_default())
                .collect::<Vec<_>>();
            assert_eq!(names, vec!["C", "A", "B"]);
            let (desired, _, _) = crate::ui::tree_sync::desired_tabs(app, cx);
            assert_eq!(desired.len(), 3, "reorder does not merge or drop a tab");
        });
    }

    #[cfg(unix)]
    #[gpui::test]
    fn tab_strip_icon_drag_rejects_multi_pane_merge_without_tree_mutation(
        cx: &mut gpui::TestAppContext,
    ) {
        let (app, mut visual) = crate::ui::app::test_window::harness(cx);
        top_tab_strip(&mut visual);
        app.update_in(&mut visual, |app, window, cx| {
            let (target_a, _a) = crate::terminal::view::quiet_test_pane(81, window, cx);
            let (target_b, _b) = crate::terminal::view::quiet_test_pane(82, window, cx);
            let (source, _s) = crate::terminal::view::quiet_test_pane(83, window, cx);
            let mut target = Tab::new(crate::ui::pane::Pane::split_node(
                Axis::Horizontal,
                0.5,
                crate::ui::pane::Pane::leaf(crate::ui::pane::PaneSlot::Ready(target_a)),
                crate::ui::pane::Pane::leaf(crate::ui::pane::PaneSlot::Ready(target_b)),
            ));
            target.name = Some("target-split".into());
            let mut source = Tab::new(crate::ui::pane::Pane::leaf(
                crate::ui::pane::PaneSlot::Ready(source),
            ));
            source.name = Some("source".into());
            app.tabs = vec![target, source];
            app.active = 0;
            app.focus_active(window, cx);
            cx.notify();
        });
        visual.run_until_parked();

        drag_selectors(&mut visual, "TAB_CHIP_1_ICON", "TAB_CHIP_0_ICON");

        app.update_in(&mut visual, |app, _, _| {
            assert_eq!(
                app.tabs.len(),
                2,
                "over-capacity merge leaves both tabs intact"
            );
            assert_eq!(app.tabs[0].pane.leaves().len(), 2);
            assert_eq!(app.tabs[1].pane.leaves().len(), 1);
        });
    }

    #[cfg(unix)]
    #[gpui::test]
    fn tab_strip_body_drag_source_before_target_maps_original_identity(
        cx: &mut gpui::TestAppContext,
    ) {
        let (app, mut visual) = crate::ui::app::test_window::harness(cx);
        top_tab_strip(&mut visual);
        install_quiet_tabs(&app, &mut visual, &[91, 92, 93], &["A", "B", "C"]);
        visual.run_until_parked();

        drag_selectors(&mut visual, "TAB_CHIP_1_BODY", "TAB_CHIP_2_BODY");

        app.update_in(&mut visual, |app, _, _| {
            assert_eq!(
                app.tabs
                    .iter()
                    .map(|tab| tab.name.clone().unwrap_or_default())
                    .collect::<Vec<_>>(),
                vec!["A", "C", "B"],
                "B→C must map the original target identity after source removal"
            );
            assert!(app.tabs.iter().all(|tab| tab.pane.leaves().len() == 1));
        });
    }

    #[cfg(unix)]
    #[gpui::test]
    fn tab_strip_pointer_drag_does_not_cross_environment_window_boundary(
        cx: &mut gpui::TestAppContext,
    ) {
        let (app, mut visual) = crate::ui::app::test_window::harness(cx);
        top_tab_strip(&mut visual);
        install_quiet_tabs(&app, &mut visual, &[101, 102], &["source", "target"]);
        visual.run_until_parked();

        let source = selector_center(&mut visual, "TAB_CHIP_0_ICON");
        let outside = gpui::point(px(-100.), px(-100.));
        visual.simulate_mouse_move(source, None, gpui::Modifiers::none());
        visual.simulate_mouse_down(source, MouseButton::Left, gpui::Modifiers::none());
        visual.simulate_mouse_move(outside, Some(MouseButton::Left), gpui::Modifiers::none());
        visual.simulate_mouse_up(outside, MouseButton::Left, gpui::Modifiers::none());
        visual.run_until_parked();
        app.update_in(&mut visual, |app, _, _| {
            assert_eq!(
                app.tabs.len(),
                2,
                "dragging outside the strip cannot detach a tab"
            );
        });
    }

    #[test]
    fn tab_strip_drag_updates_split_right_intent() {
        let source = include_str!("tab_strip.rs");
        let prod = source.split("#[cfg(test)]").next().unwrap_or(source);
        assert!(
            prod.contains("TabDragIntent::Merge")
                && prod.contains("TabDragIntent::Reorder")
                && prod.contains("set_tab_pending")
                && prod.contains("TabDragHitZone::Icon")
                && prod.contains("TabDragHitZone::Body"),
            "tab strip drag should route through typed icon/body intents"
        );
    }

    #[cfg(unix)]
    #[gpui::test]
    fn tab_strip_icon_drag_inactive_source_preserves_target_tree(cx: &mut gpui::TestAppContext) {
        use crate::terminal::view::quiet_test_pane;
        use crate::ui::pane::{Pane, PaneSlot};

        let (app, mut visual) = crate::ui::app::test_window::harness(cx);
        visual.update(|_, cx| {
            let mut config = cx.global::<crate::core::config::Config>().clone();
            config.tab_bar_position = crate::core::config::TabBarPosition::Top;
            config.cursor_blink = false;
            cx.set_global(config);
        });
        let (target_id, source_id, target_tree_id, source_tree_id, _streams) =
            app.update_in(&mut visual, |app, window, cx| {
                let (target, target_stream) = quiet_test_pane(41, window, cx);
                let (source, source_stream) = quiet_test_pane(42, window, cx);
                let target_id = target.entity_id();
                let source_id = source.entity_id();
                let mut target_tab = Tab::new(Pane::leaf(PaneSlot::Ready(target)));
                target_tab.name = Some("drop target".into());
                let mut source_tab = Tab::new(Pane::leaf(PaneSlot::Ready(source)));
                source_tab.name = Some("inactive source".into());
                let target_tree_id = target_tab.tree_id.get();
                let source_tree_id = source_tab.tree_id.get();
                app.tabs = vec![target_tab, source_tab];
                app.active = 0;
                app.focus_active(window, cx);
                cx.notify();
                (
                    target_id,
                    source_id,
                    target_tree_id,
                    source_tree_id,
                    (target_stream, source_stream),
                )
            });
        visual.run_until_parked();
        app.update_in(&mut visual, |app, window, cx| {
            let target = app.tabs[0]
                .pane
                .first_leaf()
                .expect("target tab owns one terminal");
            let source = app.tabs[1]
                .pane
                .first_leaf()
                .expect("source tab owns one terminal");
            assert!(
                target.contains_focused(window, cx),
                "the active target owns focus before dragging"
            );
            assert!(
                !source.contains_focused(window, cx),
                "the inactive source must not be pre-focused before the drag"
            );
        });

        let target = visual
            .debug_bounds("TAB_CHIP_0_ICON")
            .expect("production target chip must render");
        let source = visual
            .debug_bounds("TAB_CHIP_1_ICON")
            .expect("production source chip must render");
        let source_point = gpui::point(
            source.origin.x + source.size.width / 2.,
            source.origin.y + source.size.height / 2.,
        );
        let threshold_point = gpui::point(source_point.x - px(16.), source_point.y);
        let target_point = gpui::point(
            target.origin.x + target.size.width / 2.,
            target.origin.y + target.size.height / 2.,
        );

        visual.simulate_mouse_move(source_point, None, gpui::Modifiers::none());
        visual.simulate_mouse_down(source_point, MouseButton::Left, gpui::Modifiers::none());
        assert_eq!(
            app.update_in(&mut visual, |app, _, _| (app.active, app.tabs.len())),
            (0, 2),
            "pointer-down must neither activate nor remove the inactive source"
        );
        visual.simulate_mouse_move(
            threshold_point,
            Some(MouseButton::Left),
            gpui::Modifiers::none(),
        );
        visual.run_until_parked();
        assert!(
            visual.update(|_, cx| cx.has_active_drag()),
            "movement beyond the toolkit threshold starts the production chip drag"
        );
        assert_eq!(
            app.update_in(&mut visual, |app, _, _| (app.active, app.tabs.len())),
            (0, 2),
            "arming the drag must not activate or detach the inactive source"
        );
        visual.simulate_mouse_move(
            target_point,
            Some(MouseButton::Left),
            gpui::Modifiers::none(),
        );
        visual.run_until_parked();
        assert_eq!(
            app.update_in(&mut visual, |app, _, _| (app.active, app.tabs.len())),
            (0, 2),
            "hovering the drop target keeps the operation pending until pointer-up"
        );
        visual.simulate_mouse_up(target_point, MouseButton::Left, gpui::Modifiers::none());
        visual.run_until_parked();

        app.update_in(&mut visual, |app, window, cx| {
            assert_eq!(
                app.tabs.len(),
                1,
                "drop commits one canonical top-level tab"
            );
            assert_eq!(app.active, 0);
            let tab = &app.tabs[0];
            assert_eq!(tab.name.as_deref(), Some("drop target"));
            assert_eq!(
                tab.tree_id.get(),
                target_tree_id,
                "the target canonical tree identity survives the drop"
            );
            assert_ne!(
                tab.tree_id.get(),
                source_tree_id,
                "the source must not replace the target tree"
            );
            match &tab.pane {
                Pane::Split { axis, .. } => assert_eq!(
                    *axis,
                    Axis::Horizontal,
                    "drop uses the canonical right-split axis"
                ),
                _ => panic!("the target leaf must become one canonical split tree"),
            }
            let pane_leaves = tab.pane.leaves();
            let leaves = pane_leaves
                .iter()
                .map(|leaf| leaf.entity_id())
                .collect::<Vec<_>>();
            assert_eq!(
                leaves,
                vec![target_id, source_id],
                "the exact target stays left and the exact source moves right without respawn"
            );
            assert!(
                pane_leaves[1].contains_focused(window, cx),
                "the moved source leaf receives focus after canonical split commit"
            );
            let (desired, active, held) = crate::ui::tree_sync::desired_tabs(app, cx);
            assert_eq!(desired.len(), 1, "one canonical desired tree is persisted");
            assert_eq!(desired[0].id, target_tree_id);
            assert_eq!(active, Some(target_tree_id));
            assert!(
                held.is_empty(),
                "no parallel or unmaterialized tree is held"
            );
            match &desired[0].root {
                crate::ui::tree_sync::DesiredNode::Split { axis, ratio, a, b } => {
                    assert_eq!(*axis, agentty_core::core::machine::Axis::Horizontal);
                    assert_eq!(*ratio, 0.5);
                    assert!(matches!(
                        &**a,
                        crate::ui::tree_sync::DesiredNode::Leaf { pane: 41, .. }
                    ));
                    assert!(matches!(
                        &**b,
                        crate::ui::tree_sync::DesiredNode::Leaf { pane: 42, .. }
                    ));
                }
                _ => panic!("persisted target tree must contain the canonical right split"),
            }
        });
    }

    #[test]
    fn tab_strip_activation_is_click_only() {
        let source = include_str!("tab_strip.rs");
        let prod = source.split("#[cfg(test)]").next().unwrap_or(source);
        let mut cursor = 0usize;
        while let Some(offset) = prod[cursor..].find("on_mouse_down(") {
            let start = cursor + offset;
            let open_offset = prod[start..]
                .find('(')
                .unwrap_or_else(|| panic!("expected opening paren for on_mouse_down at {start}"));
            let mut depth = 0i32;
            let mut end = None;
            for (idx, ch) in prod[start + open_offset..].char_indices() {
                match ch {
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            end = Some(start + open_offset + idx + 1);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            let end = end.unwrap_or_else(|| {
                panic!("could not parse on_mouse_down call ending for production slice at {start}")
            });
            let chunk = &prod[start..end];
            assert!(
                !chunk.contains("activate("),
                "activation must be click-driven for tab-strip chips and labels; chunk: {chunk}",
            );
            cursor = end;
        }

        assert!(
            prod.contains("this.activate(i, window, cx);"),
            "tab strip should still have explicit click activation path for chips"
        );
        assert!(
            prod.contains("on_click(cx.listener(move |this, _, window, cx|"),
            "tab strip activation path should remain explicit"
        );
    }

    #[gpui::test]
    fn inactive_tab_pointer_drag_cancels_activation_and_click_release_activates(
        cx: &mut gpui::TestAppContext,
    ) {
        let (app, mut visual) = crate::ui::app::test_window::harness(cx);
        visual.update(|_, cx| {
            let mut config = cx.global::<crate::core::config::Config>().clone();
            config.tab_bar_position = crate::core::config::TabBarPosition::Top;
            cx.set_global(config);
        });
        app.update_in(&mut visual, |app, _window, cx| {
            let mut first = Tab::new(crate::ui::pane::Pane::Empty);
            first.name = Some("active".into());
            let mut second = Tab::new(crate::ui::pane::Pane::Empty);
            second.name = Some("inactive".into());
            app.tabs = vec![first, second];
            app.active = 0;
            cx.notify();
        });
        visual.run_until_parked();

        let inactive = visual
            .debug_bounds("TAB_CHIP_1")
            .expect("inactive production tab chip must render");
        let press = gpui::point(
            inactive.origin.x + inactive.size.width / 2.,
            inactive.origin.y + inactive.size.height / 2.,
        );
        let dragged = gpui::point(press.x + px(16.), press.y);

        visual.simulate_mouse_move(press, None, gpui::Modifiers::none());
        visual.simulate_mouse_down(press, MouseButton::Left, gpui::Modifiers::none());
        assert_eq!(
            app.update_in(&mut visual, |app, _, _| app.active),
            0,
            "pointer-down alone must not activate an inactive tab"
        );
        visual.simulate_mouse_move(dragged, Some(MouseButton::Left), gpui::Modifiers::none());
        visual.run_until_parked();
        assert!(
            visual.update(|_, cx| cx.has_active_drag()),
            "movement beyond GPUI's drag threshold must enter the drag path"
        );
        assert_eq!(
            app.update_in(&mut visual, |app, _, _| app.active),
            0,
            "starting a drag from an inactive tab must not activate it"
        );
        visual.simulate_mouse_up(dragged, MouseButton::Left, gpui::Modifiers::none());
        visual.run_until_parked();
        assert_eq!(
            app.update_in(&mut visual, |app, _, _| app.active),
            0,
            "releasing a completed drag must not replay tab activation"
        );

        let inactive = visual
            .debug_bounds("TAB_CHIP_1")
            .expect("inactive production tab chip must remain rendered");
        let click = gpui::point(
            inactive.origin.x + inactive.size.width / 2.,
            inactive.origin.y + inactive.size.height / 2.,
        );
        visual.simulate_mouse_move(click, None, gpui::Modifiers::none());
        visual.simulate_mouse_down(click, MouseButton::Left, gpui::Modifiers::none());
        assert_eq!(
            app.update_in(&mut visual, |app, _, _| app.active),
            0,
            "a click still waits for release before activation"
        );
        visual.simulate_mouse_up(click, MouseButton::Left, gpui::Modifiers::none());
        visual.run_until_parked();
        assert_eq!(
            app.update_in(&mut visual, |app, _, _| app.active),
            1,
            "a stationary click-release must activate the inactive tab"
        );
    }

    #[test]
    fn tab_chip_hierarchy_elevates_active_tab_with_indicator() {
        let active = tab_chip_hierarchy(true);
        assert!(active.elevated, "active tab must be an elevated surface");
        assert!(
            active.indicator,
            "active tab must show the accent indicator"
        );
        let inactive = tab_chip_hierarchy(false);
        assert!(!inactive.elevated, "inactive tabs stay flat");
        assert!(
            !inactive.indicator,
            "inactive tabs never show the accent indicator"
        );
    }

    #[test]
    fn tab_drag_target_after_removal_maps_source_before_and_after_target() {
        assert_eq!(target_after_removal(1, 1, 3), Some(2));
        assert_eq!(target_after_removal(2, 0, 3), Some(0));
        assert_eq!(target_after_removal(1, 0, 3), Some(0));
        assert_eq!(target_after_removal(0, 2, 3), None);
    }

    #[test]
    fn tab_drag_order_for_target_preserves_original_target_identity() {
        assert_eq!(order_for_hover(2, 0, 3), Some(vec![2, 0, 1]));
        assert_eq!(order_for_hover(1, 2, 3), Some(vec![0, 2, 1]));
        assert_eq!(order_for_hover(1, 1, 3), None);
    }

    #[test]
    fn tab_chip_window_compresses_before_overflowing() {
        // Wide enough for every chip at the natural minimum: no overflow.
        let wide = tab_chip_window(1200., 5, 2);
        assert_eq!(wide.visible, 5);
        assert_eq!(wide.chip_min_w, CHIP_MIN_NATURAL);
        assert_eq!((wide.leading_hidden, wide.trailing_hidden), (0, 0));

        // Too narrow for the natural minimum but wide enough at the floor:
        // compress first, still no overflow.
        let squeezed = tab_chip_window(400., 5, 2);
        assert_eq!(squeezed.visible, 5);
        assert_eq!(squeezed.chip_min_w, CHIP_MIN_FLOOR);
        assert_eq!((squeezed.leading_hidden, squeezed.trailing_hidden), (0, 0));

        // Below the floor capacity: overflow with an affordance count.
        let clipped = tab_chip_window(200., 10, 0);
        assert!(clipped.visible < 10);
        assert_eq!(clipped.chip_min_w, CHIP_MIN_FLOOR);
        assert_eq!(clipped.leading_hidden, 0);
        assert_eq!(clipped.trailing_hidden, 10 - clipped.visible);
        assert!(clipped.trailing_hidden > 0);
    }

    #[test]
    fn tab_chip_window_keeps_active_tab_visible() {
        for avail in [120., 200., 320., 515.] {
            for count in 1..24usize {
                for active in 0..count {
                    let w = tab_chip_window(avail, count, active);
                    assert!(w.visible >= 1, "avail={avail} count={count}");
                    assert!(
                        w.start <= active && active < w.start + w.visible,
                        "avail={avail} count={count} active={active}: window {:?} hides the active tab",
                        w
                    );
                    assert_eq!(w.start, w.leading_hidden);
                    assert_eq!(w.start + w.visible + w.trailing_hidden, count);
                }
            }
        }
        // Middle-active overflow pages both directions.
        let mid = tab_chip_window(200., 10, 5);
        assert!(mid.start <= 5 && 5 < mid.start + mid.visible);
        assert!(
            mid.leading_hidden > 0,
            "expected hidden tabs before the window"
        );
        assert!(
            mid.trailing_hidden > 0,
            "expected hidden tabs after the window"
        );
    }
}
