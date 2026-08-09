use gpui::{
    AnyElement, ElementId, Hsla, ListState, div, linear_color_stop, linear_gradient, prelude::*, px,
};
use gpui_component::scroll::{Scrollbar, ScrollbarHandle, ScrollbarShow};
use gpui_component::v_flex;

/// Side-panel lists auto-hide the thumb (UI-SIDE-PANEL-SCROLL-11).
pub(crate) const SIDE_PANEL_SCROLLBAR_SHOW: ScrollbarShow = ScrollbarShow::Scrolling;

/// Bottom overflow fade height for "more content below" hint.
pub(crate) const SIDE_PANEL_OVERFLOW_FADE_HEIGHT: f32 = 28.0;

/// True when a GPUI list can scroll further down.
pub(crate) fn list_has_overflow_below(list: &ListState) -> bool {
    let max = list.max_offset_for_scrollbar().y;
    if max <= px(0.) {
        return false;
    }
    let scrolled = -list.scroll_px_offset_for_scrollbar().y;
    scrolled < max - px(0.5)
}

pub(crate) fn with_vertical_scrollbar<H: ScrollbarHandle + Clone>(
    id: impl Into<ElementId>,
    scroll_area: impl IntoElement,
    handle: &H,
) -> AnyElement {
    with_vertical_scrollbar_overflow(id, scroll_area, handle, false, gpui::transparent_black())
}

/// Side-panel scroll surface: auto-hide scrollbar + optional bottom overflow fade.
pub(crate) fn with_vertical_scrollbar_overflow<H: ScrollbarHandle + Clone>(
    id: impl Into<ElementId>,
    scroll_area: impl IntoElement,
    handle: &H,
    more_below: bool,
    fade_to: Hsla,
) -> AnyElement {
    let mut clear = fade_to;
    clear.a = 0.;
    v_flex()
        .relative()
        .flex_1()
        .min_h_0()
        .child(scroll_area)
        .when(more_below, |this| {
            this.child(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .bottom_0()
                    .h(px(SIDE_PANEL_OVERFLOW_FADE_HEIGHT))
                    .bg(linear_gradient(
                        180.,
                        linear_color_stop(clear, 0.),
                        linear_color_stop(fade_to, 1.),
                    )),
            )
        })
        .child(
            div()
                .absolute()
                .top_0()
                .left_0()
                .right_0()
                .bottom_0()
                .child(
                    Scrollbar::vertical(handle)
                        .id(id)
                        .scrollbar_show(SIDE_PANEL_SCROLLBAR_SHOW),
                ),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn side_panel_scrollbar_forces_scrolling_show_mode() {
        assert_eq!(SIDE_PANEL_SCROLLBAR_SHOW, ScrollbarShow::Scrolling);
        assert!(SIDE_PANEL_OVERFLOW_FADE_HEIGHT >= 16.0);
    }

    #[test]
    fn overflow_fade_geometry_is_explicit() {
        assert_eq!(SIDE_PANEL_OVERFLOW_FADE_HEIGHT, 28.0);
    }
}
