//! Shared text-truncation and text-input rendering helpers for views.

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Truncate `s` so its displayed (unicode) width is at most `max_width`,
/// appending an ellipsis when truncation occurs.
pub fn truncate_to_display_width(s: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }
    if UnicodeWidthStr::width(s) <= max_width {
        return s.to_string();
    }
    if max_width <= 1 {
        return "…".to_string();
    }
    let mut out = String::new();
    let mut used = 0usize;
    for ch in s.chars() {
        let w = ch.width().unwrap_or(0);
        if used + w + 1 > max_width {
            break;
        }
        used += w;
        out.push(ch);
    }
    out.push('…');
    out
}

/// Render a `label: value█` input line, truncating `value` to fit `width`.
/// The cursor block is only drawn when `focused` is true, so the same helper
/// serves both an actively-edited field and a read-only preview of it.
#[allow(clippy::too_many_arguments)]
pub fn render_text_input_line(
    label: &str,
    value: &str,
    width: u16,
    focused: bool,
    label_style: Style,
    value_style: Style,
    cursor_style: Style,
) -> Line<'static> {
    let cursor = if focused { "█" } else { "" };
    let max_value_width = width
        .saturating_sub(
            UnicodeWidthStr::width(label) as u16 + UnicodeWidthStr::width(cursor) as u16,
        )
        .max(1) as usize;
    let display_value = truncate_to_display_width(value, max_value_width);
    Line::from(vec![
        Span::styled(label.to_string(), label_style),
        Span::styled(display_value, value_style),
        Span::styled(cursor.to_string(), cursor_style),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_adds_ellipsis_and_respects_display_width() {
        let s = truncate_to_display_width("src/very/long/path/to/file.rs", 10);
        assert!(UnicodeWidthStr::width(s.as_str()) <= 10);
        assert!(s.ends_with('…'));

        assert_eq!(truncate_to_display_width("hi", 10), "hi");
        assert_eq!(truncate_to_display_width("abc", 0), "");
    }

    #[test]
    fn truncate_narrow_uses_ellipsis() {
        assert_eq!(truncate_to_display_width("hello world", 1), "…");
    }

    #[test]
    fn truncate_fits_unchanged() {
        assert_eq!(truncate_to_display_width("hi", 5), "hi");
    }
}
