//! The main window's content for each [`Category`]: the overview with the app's commands,
//! and for every cleaning area its header, purpose and scope.

use gpui_kit::component::{ActiveTheme as _, Icon, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Action, AnyElement, App, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, img,
};

use crate::actions::{self, OpenSettings, Quit, ToggleAppearance, ToggleSidebar};
use crate::brand;
use crate::nav::Category;
use crate::tokens::{chrome, layout, space, text};

/// Content for `category`.
pub(crate) fn render(category: Category, window: &Window, cx: &App) -> AnyElement {
    match category {
        Category::Overview => overview(window, cx),
        _ => area(category, cx),
    }
}

fn overview(window: &Window, cx: &App) -> AnyElement {
    let muted = cx.theme().muted_foreground;
    let shortcuts = [
        ("nav.settings", &OpenSettings as &dyn Action),
        ("nav.toggle_sidebar", &ToggleSidebar),
        ("appearance.toggle", &ToggleAppearance),
        ("main.quit", &Quit),
    ]
    .into_iter()
    .map(|(label, action)| shortcut_row(tr(label), action, window, cx))
    .collect::<Vec<_>>();

    v_flex()
        .id("page-overview")
        .flex_1()
        .min_h_0()
        .w_full()
        .overflow_y_scroll()
        .items_center()
        .justify_center()
        .p(space::XXL)
        .child(
            v_flex()
                .w(chrome::EMPTY_STATE_WIDTH)
                .max_w_full()
                .gap(space::XXL)
                .child(
                    v_flex()
                        .gap(space::SM)
                        .child(
                            h_flex()
                                .gap(space::LG)
                                .child(img(brand::mark()).flex_none().size(layout::PAGE_ICON_TILE))
                                .child(
                                    div()
                                        .text_size(text::DISPLAY)
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child("oh-my-clear"),
                                ),
                        )
                        .child(
                            div()
                                .text_size(text::BODY)
                                .line_height(text::BODY_LINE_HEIGHT)
                                .text_color(muted)
                                .child(tr("app.tagline")),
                        ),
                )
                .child(
                    v_flex()
                        .gap(space::XS)
                        .child(
                            div()
                                .text_size(text::CAPTION)
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(muted)
                                .child(tr("main.shortcuts")),
                        )
                        .children(shortcuts),
                ),
        )
        .into_any_element()
}

fn area(category: Category, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let header = h_flex()
        .gap(space::LG)
        .child(
            div()
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .size(layout::PAGE_ICON_TILE)
                .rounded(theme.radius)
                .bg(theme.secondary)
                .text_color(theme.secondary_foreground)
                .child(Icon::new(category.icon()).size(chrome::ICON)),
        )
        .child(
            div()
                .text_size(text::DISPLAY)
                .font_weight(FontWeight::SEMIBOLD)
                .child(category.title()),
        );

    v_flex()
        .id(SharedString::from(format!("page-{}", category.key())))
        .flex_1()
        .min_h_0()
        .w_full()
        .overflow_y_scroll()
        .p(space::XXL)
        .child(
            v_flex()
                .w(chrome::EMPTY_STATE_WIDTH)
                .max_w_full()
                .gap(space::XL)
                .child(header)
                .when_some(category.description(), |this, description| {
                    this.child(
                        div()
                            .text_size(text::BODY)
                            .line_height(text::BODY_LINE_HEIGHT)
                            .text_color(theme.muted_foreground)
                            .child(description),
                    )
                })
                .when_some(category.covers(), |this, covers| {
                    this.child(
                        v_flex()
                            .gap(space::XS)
                            .child(
                                div()
                                    .text_size(text::CAPTION)
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme.muted_foreground)
                                    .child(tr("page.covers")),
                            )
                            .child(
                                div()
                                    .text_size(text::BODY)
                                    .line_height(text::BODY_LINE_HEIGHT)
                                    .text_color(theme.foreground)
                                    .child(covers),
                            ),
                    )
                }),
        )
        .into_any_element()
}

fn shortcut_row(label: SharedString, action: &dyn Action, window: &Window, cx: &App) -> AnyElement {
    h_flex()
        .justify_between()
        .py(space::XS)
        .child(
            div()
                .text_size(text::BODY)
                .text_color(cx.theme().foreground)
                .child(label),
        )
        .child(
            h_flex()
                .gap(space::XS)
                .children(actions::key_hints(action, window)),
        )
        .into_any_element()
}

fn tr(key: &str) -> SharedString {
    rust_i18n::t!(key).to_string().into()
}
