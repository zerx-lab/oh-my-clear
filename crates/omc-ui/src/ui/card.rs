//! [`Card`] and [`CardHeader`]: the surface every content section sits on.

use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, FontWeight, IntoElement, ParentElement, Refineable as _, RenderOnce,
    SharedString, StyleRefinement, Styled, Window, div,
};

use crate::tokens::{card, space, text};

/// A content section: surface fill, hairline border, card radius, no shadow. Children are
/// padded unless [`Self::flush`] (lists that run edge to edge, with their own row padding).
#[derive(IntoElement)]
pub struct Card {
    header: Option<CardHeader>,
    flush: bool,
    style: StyleRefinement,
    children: Vec<AnyElement>,
}

impl Card {
    /// An empty padded card.
    pub fn new() -> Self {
        Self {
            header: None,
            flush: false,
            style: StyleRefinement::default(),
            children: Vec::new(),
        }
    }

    /// Adds a header row above the content, separated by a hairline.
    #[must_use]
    pub fn header(mut self, header: CardHeader) -> Self {
        self.header = Some(header);
        self
    }

    /// Drops the content padding.
    #[must_use]
    pub fn flush(mut self) -> Self {
        self.flush = true;
        self
    }
}

impl Default for Card {
    fn default() -> Self {
        Self::new()
    }
}

impl ParentElement for Card {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl Styled for Card {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for Card {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let hairline = theme.border.alpha(theme.border.a * card::BORDER_ALPHA);
        let has_children = !self.children.is_empty();
        let mut root = v_flex()
            .w_full()
            .min_w_0()
            .bg(theme.group_box)
            .text_color(theme.group_box_foreground)
            .border_1()
            .border_color(hairline)
            .rounded(theme.radius_lg)
            .when_some(self.header, |this, header| {
                this.child(
                    div()
                        .when(has_children, |this| {
                            this.border_b_1().border_color(hairline)
                        })
                        .child(header),
                )
            })
            .when(has_children, |this| {
                this.child(
                    v_flex()
                        .flex_1()
                        .min_h_0()
                        .w_full()
                        .when(!self.flush, |this| this.p(card::PADDING).gap(space::MD))
                        .children(self.children),
                )
            });
        root.style().refine(&self.style);
        root
    }
}

/// A card's title row: title 14/600, optional description, and a trailing slot (summary
/// stats, actions).
#[derive(IntoElement)]
pub struct CardHeader {
    title: SharedString,
    description: Option<SharedString>,
    leading: Option<AnyElement>,
    trailing: Vec<AnyElement>,
}

impl CardHeader {
    /// A header titled `title`.
    pub fn new(title: impl Into<SharedString>) -> Self {
        Self {
            title: title.into(),
            description: None,
            leading: None,
            trailing: Vec::new(),
        }
    }

    /// One muted line under the title.
    #[must_use]
    pub fn description(mut self, description: impl Into<SharedString>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// An element before the title (icon, checkbox).
    #[must_use]
    pub fn leading(mut self, element: impl IntoElement) -> Self {
        self.leading = Some(element.into_any_element());
        self
    }

    /// Appends an element to the right-aligned slot.
    #[must_use]
    pub fn trailing(mut self, element: impl IntoElement) -> Self {
        self.trailing.push(element.into_any_element());
        self
    }
}

impl RenderOnce for CardHeader {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        h_flex()
            .w_full()
            .gap(space::LG)
            .px(card::PADDING)
            .py(card::HEADER_PAD_Y)
            .children(self.leading)
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .text_size(text::SECTION)
                            .line_height(text::SECTION_LINE_HEIGHT)
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(self.title),
                    )
                    .when_some(self.description, |this, description| {
                        this.child(
                            div()
                                .text_size(text::SMALL)
                                .line_height(text::SMALL_LINE_HEIGHT)
                                .text_color(muted)
                                .child(description),
                        )
                    }),
            )
            .when(!self.trailing.is_empty(), |this| {
                this.child(h_flex().flex_none().gap(space::XL).children(self.trailing))
            })
    }
}
