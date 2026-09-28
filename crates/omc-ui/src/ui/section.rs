//! Page structure: [`PageHeader`], [`SectionHeader`], [`Toolbar`].

use gpui_kit::component::{ActiveTheme as _, Icon, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, FontWeight, IntoElement, ParentElement, Refineable as _, RenderOnce,
    SharedString, StyleRefinement, Styled, Window, div,
};

use crate::tokens::{chrome, layout, radius, space, text};

/// Top of a page: 28 px icon tile and 20/600 title on one line, the page toolbar right-
/// aligned on that line (children), and a 13 px muted description below.
#[derive(IntoElement)]
pub struct PageHeader {
    icon: Icon,
    title: SharedString,
    description: Option<SharedString>,
    actions: Vec<AnyElement>,
}

impl PageHeader {
    /// A header for the page titled `title`.
    pub fn new(icon: impl Into<Icon>, title: impl Into<SharedString>) -> Self {
        Self {
            icon: icon.into(),
            title: title.into(),
            description: None,
            actions: Vec::new(),
        }
    }

    /// One-sentence description under the title.
    #[must_use]
    pub fn description(mut self, description: impl Into<SharedString>) -> Self {
        self.description = Some(description.into());
        self
    }
}

impl ParentElement for PageHeader {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.actions.extend(elements);
    }
}

impl RenderOnce for PageHeader {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        v_flex()
            .w_full()
            .gap(space::XS)
            .child(
                h_flex()
                    .w_full()
                    .gap(space::LG)
                    .child(
                        div()
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(layout::PAGE_ICON_TILE)
                            .rounded(radius::outer(theme.radius))
                            .bg(theme.muted)
                            .text_color(theme.foreground)
                            .child(self.icon.size(chrome::ICON)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_size(text::PAGE_TITLE)
                            .line_height(text::PAGE_TITLE_LINE_HEIGHT)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.foreground)
                            .child(self.title),
                    )
                    .when(!self.actions.is_empty(), |this| {
                        this.child(h_flex().flex_none().gap(space::MD).children(self.actions))
                    }),
            )
            .when_some(self.description, |this, description| {
                this.child(
                    div()
                        .text_size(text::BODY)
                        .line_height(text::BODY_LINE_HEIGHT)
                        .text_color(theme.muted_foreground)
                        .child(description),
                )
            })
    }
}

/// A titled group without a card of its own: 14/600 title, optional 12 px description,
/// trailing controls (children).
#[derive(IntoElement)]
pub struct SectionHeader {
    title: SharedString,
    description: Option<SharedString>,
    trailing: Vec<AnyElement>,
}

impl SectionHeader {
    /// A header titled `title`.
    pub fn new(title: impl Into<SharedString>) -> Self {
        Self {
            title: title.into(),
            description: None,
            trailing: Vec::new(),
        }
    }

    /// One muted line under the title.
    #[must_use]
    pub fn description(mut self, description: impl Into<SharedString>) -> Self {
        self.description = Some(description.into());
        self
    }
}

impl ParentElement for SectionHeader {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.trailing.extend(elements);
    }
}

impl RenderOnce for SectionHeader {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        h_flex()
            .w_full()
            .gap(space::LG)
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .text_size(text::SECTION)
                            .line_height(text::SECTION_LINE_HEIGHT)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.foreground)
                            .child(self.title),
                    )
                    .when_some(self.description, |this, description| {
                        this.child(
                            div()
                                .text_size(text::SMALL)
                                .line_height(text::SMALL_LINE_HEIGHT)
                                .text_color(theme.muted_foreground)
                                .child(description),
                        )
                    }),
            )
            .when(!self.trailing.is_empty(), |this| {
                this.child(h_flex().flex_none().gap(space::MD).children(self.trailing))
            })
    }
}

/// A horizontal run of controls on one baseline with the standard 8 px gap. Styled, so a
/// page can `.justify_end()` or `.flex_1()` it.
#[derive(IntoElement)]
pub struct Toolbar {
    style: StyleRefinement,
    children: Vec<AnyElement>,
}

impl Toolbar {
    /// An empty toolbar.
    pub fn new() -> Self {
        Self {
            style: StyleRefinement::default(),
            children: Vec::new(),
        }
    }
}

impl Default for Toolbar {
    fn default() -> Self {
        Self::new()
    }
}

impl ParentElement for Toolbar {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl Styled for Toolbar {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for Toolbar {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let mut root = h_flex().flex_wrap().gap(space::MD).children(self.children);
        root.style().refine(&self.style);
        root
    }
}
