//! [`RowGrid`]: the leading columns shared by list rows and group headers.

use gpui_kit::component::h_flex;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{AnyElement, Div, ParentElement as _, Pixels, Styled as _, div, px};

use crate::tokens::row;

/// The leading columns of a list: `[disclosure][checkbox][icon] title`, each a 16 px cell
/// 8 px apart (`tokens::row::{SLOT, SLOT_GAP}`).
///
/// Every row of one list — group headers and their items — takes the same grid, so a
/// column a row has nothing for stays as an empty cell: the header's checkbox and every
/// item's checkbox share one x, and item titles start where the header title starts.
/// [`crate::ui::CollapsibleHeader`] fills the disclosure cell; rows leave it empty.
///
/// ```ignore
/// const GRID: ui::RowGrid = ui::RowGrid::new().disclosure().check().icon();
/// ui::CollapsibleHeader::new(..).grid(GRID).checkbox(..)
/// ui::ListRow::new(..).grid(GRID).checkbox(..).icon(ui::row_icon(..))
/// ```
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RowGrid {
    disclosure: bool,
    check: bool,
    icon: bool,
}

impl RowGrid {
    /// No leading columns.
    pub const fn new() -> Self {
        Self {
            disclosure: false,
            check: false,
            icon: false,
        }
    }

    /// Adds the disclosure (chevron) column of group headers.
    #[must_use]
    pub const fn disclosure(mut self) -> Self {
        self.disclosure = true;
        self
    }

    /// Adds the checkbox column.
    #[must_use]
    pub const fn check(mut self) -> Self {
        self.check = true;
        self
    }

    /// Adds the 16 px icon column.
    #[must_use]
    pub const fn icon(mut self) -> Self {
        self.icon = true;
        self
    }

    /// The columns of both grids.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self {
            disclosure: self.disclosure || other.disclosure,
            check: self.check || other.check,
            icon: self.icon || other.icon,
        }
    }

    /// Whether the grid has no column.
    pub const fn is_empty(self) -> bool {
        !(self.disclosure || self.check || self.icon)
    }

    /// Distance from a row's content edge to its checkbox column, when it has one.
    pub fn check_offset(self) -> Option<Pixels> {
        self.check.then(|| offset(u8::from(self.disclosure)))
    }

    /// Distance from a row's content edge to its title.
    pub fn title_offset(self) -> Pixels {
        offset(
            u8::from(self.disclosure)
                .saturating_add(u8::from(self.check))
                .saturating_add(u8::from(self.icon)),
        )
    }

    /// The leading cells, in grid order: one 16 px cell per column holding its element,
    /// or nothing (so rows without one stay aligned). An element for a column the grid
    /// lacks is dropped. Callers put [`tokens::row::SLOT_GAP`](row::SLOT_GAP) between the
    /// cells and the title (e.g. column headers of a table).
    pub fn cells(
        self,
        disclosure: Option<AnyElement>,
        check: Option<AnyElement>,
        icon: Option<AnyElement>,
    ) -> Div {
        let cell = |content: Option<AnyElement>| {
            div()
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .w(row::SLOT)
                .children(content)
        };
        h_flex()
            .flex_none()
            .gap(row::SLOT_GAP)
            .when(self.disclosure, |this| this.child(cell(disclosure)))
            .when(self.check, |this| this.child(cell(check)))
            .when(self.icon, |this| this.child(cell(icon)))
    }
}

/// Offset of the leading column `index` (0-based): whole cells and gaps before it.
fn offset(index: u8) -> Pixels {
    px(f32::from(index) * (f32::from(row::SLOT) + f32::from(row::SLOT_GAP)))
}

#[cfg(test)]
mod tests {
    use gpui_kit::px;

    use super::RowGrid;

    #[test]
    fn titles_follow_every_column_and_checkboxes_follow_the_disclosure() {
        let grouped = RowGrid::new().disclosure().check().icon();
        assert_eq!(
            grouped.check_offset(),
            Some(px(24.)),
            "checkbox after the 16 px disclosure and its 8 px gap"
        );
        assert_eq!(
            grouped.title_offset(),
            px(72.),
            "title after three cells and gaps"
        );
        let flat = RowGrid::new().check();
        assert_eq!(
            flat.check_offset(),
            Some(px(0.)),
            "flat lists start with the checkbox"
        );
        assert_eq!(flat.title_offset(), px(24.), "one cell and its gap");
        assert_eq!(RowGrid::new().check_offset(), None, "no checkbox column");
        assert_eq!(RowGrid::new().title_offset(), px(0.), "no leading columns");
    }

    #[test]
    fn union_keeps_every_column_of_both_grids() {
        let grid = RowGrid::new().disclosure().union(RowGrid::new().icon());
        assert_eq!(
            grid,
            RowGrid::new().disclosure().icon(),
            "union is column-wise or"
        );
        assert!(RowGrid::new().is_empty(), "new grid has no columns");
        assert!(!grid.is_empty(), "union of non-empty grids is non-empty");
    }
}
