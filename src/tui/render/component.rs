use ratatui::layout::Rect;
use ratatui::Frame;

use super::theme::Theme;

/// Read-only snapshot passed to every leaf component.
pub struct RenderContext<'a> {
    pub state: &'a crate::tui::app::AppState,
    pub view: &'a crate::tui::state::ViewState,
    pub theme: &'a Theme,
}

impl<'a> RenderContext<'a> {
    pub fn new(
        state: &'a crate::tui::app::AppState,
        view: &'a crate::tui::state::ViewState,
        theme: &'a Theme,
    ) -> Self {
        Self { state, view, theme }
    }
}

/// Pure rendering trait. Implementations must not mutate `AppState` or
/// `ViewState`; all layout and scroll clamping must happen in
/// `ViewStateManager::prepare` before render.
pub trait Component {
    fn render(&self, f: &mut Frame, area: Rect, ctx: &RenderContext);
}

/// Blanket impl for functions matching the component signature.
impl<F> Component for F
where
    F: Fn(&mut Frame, Rect, &RenderContext),
{
    fn render(&self, f: &mut Frame, area: Rect, ctx: &RenderContext) {
        (self)(f, area, ctx);
    }
}
