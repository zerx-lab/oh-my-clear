//! [`Kbd`]: the key binding of an action, in the look of gpui-component's `Kbd`.

use gpui_kit::component::h_flex;
use gpui_kit::{Action, App, IntoElement, ParentElement as _, RenderOnce, Styled as _, Window};

use crate::actions;
use crate::tokens::space;

/// The keystrokes bound to an action (highest-precedence binding in the window); renders
/// nothing when the action is unbound. Put it in menus, tooltips and sidebar items.
#[derive(IntoElement)]
pub struct Kbd {
    action: Box<dyn Action>,
}

impl Kbd {
    /// Hints for `action`.
    pub fn new(action: &dyn Action) -> Self {
        Self {
            action: action.boxed_clone(),
        }
    }
}

impl RenderOnce for Kbd {
    fn render(self, window: &mut Window, _: &mut App) -> impl IntoElement {
        h_flex()
            .flex_none()
            .gap(space::XXS)
            .children(actions::key_hints(self.action.as_ref(), window))
    }
}
