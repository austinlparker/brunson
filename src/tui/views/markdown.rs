use pulldown_cmark::{Event, Parser, Tag, TagEnd};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::tui::render::theme::{CODE_BG, LINK, MUTED, TEXT};

/// Render a Markdown string as wrapped ratatui `Line`s that fit inside `width` columns.
///
/// Supported formatting:
/// - paragraphs and line breaks
/// - headings (prefixed and bold)
/// - blockquotes, bullet/ordered lists, code blocks
/// - inline emphasis, strong, strikethrough, code
/// - links (styled + visible URL fallback for labeled links)
pub fn markdown_lines(text: &str, width: usize) -> Vec<Line<'static>> {
    MarkdownBuffer::new(width.max(1)).render(text)
}

struct ListFrame {
    ordered: bool,
    next: usize,
}

#[derive(Debug)]
enum Container {
    Paragraph,
    Heading,
    BlockQuote,
    Item,
}

struct MarkdownBuffer {
    width: usize,
    base: Style,
    lines: Vec<Line<'static>>,
    current: Vec<Span<'static>>,
    current_width: usize,
    needs_space: bool,
    style_stack: Vec<Style>,
    first_prefix: String,
    cont_prefix: String,
    prefix_stack: Vec<(String, String)>,
    containers: Vec<Container>,
    list_stack: Vec<ListFrame>,
    in_code_block: bool,
    code_buffer: String,
    link_url: Option<String>,
    link_text: Option<String>,
    image_url: Option<String>,
    image_alt: Option<String>,
}

impl MarkdownBuffer {
    fn new(width: usize) -> Self {
        let base = Style::default().fg(TEXT);
        Self {
            width,
            base,
            lines: Vec::new(),
            current: Vec::new(),
            current_width: 0,
            needs_space: false,
            style_stack: vec![base],
            first_prefix: String::new(),
            cont_prefix: String::new(),
            prefix_stack: Vec::new(),
            containers: Vec::new(),
            list_stack: Vec::new(),
            in_code_block: false,
            code_buffer: String::new(),
            link_url: None,
            link_text: None,
            image_url: None,
            image_alt: None,
        }
    }

    fn render(mut self, text: &str) -> Vec<Line<'static>> {
        for event in Parser::new(text) {
            match event {
                Event::Start(tag) => self.start_tag(tag),
                Event::End(end) => self.end_tag(end),
                Event::Text(t) => self.text(&t),
                Event::Code(c) => self.code(&c),
                Event::Html(h) | Event::InlineHtml(h) => self.text(&h),
                Event::InlineMath(s) | Event::DisplayMath(s) => self.text(&s),
                Event::SoftBreak => self.soft_break(),
                Event::HardBreak => self.hard_break(),
                Event::Rule => self.rule(),
                Event::FootnoteReference(_) => {}
                Event::TaskListMarker(checked) => self.task_marker(checked),
            }
        }
        self.finish()
    }

    fn current_style(&self) -> Style {
        *self.style_stack.last().unwrap_or(&self.base)
    }

    fn start_tag(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => self.containers.push(Container::Paragraph),
            Tag::Heading { level, .. } => {
                let level = level as u8;
                let prefix = heading_prefix(level);
                self.containers.push(Container::Heading);
                self.push_prefix(&prefix, &prefix);
                self.push_style(self.base.add_modifier(Modifier::BOLD));
            }
            Tag::BlockQuote(_) => {
                self.containers.push(Container::BlockQuote);
                self.push_prefix("▌ ", "  ");
            }
            Tag::CodeBlock(_) => {
                self.in_code_block = true;
                self.code_buffer.clear();
            }
            Tag::List(start) => {
                self.list_stack.push(ListFrame {
                    ordered: start.is_some(),
                    next: start.unwrap_or(1) as usize,
                });
            }
            Tag::Item => {
                let number = self
                    .list_stack
                    .last_mut()
                    .map(|f| {
                        let n = f.next;
                        f.next += 1;
                        n
                    })
                    .unwrap_or(1);
                let (first, cont) = list_item_prefix(&self.list_stack, number);
                self.containers.push(Container::Item);
                self.push_prefix(&first, &cont);
            }
            Tag::Emphasis => self.push_style(self.current_style().add_modifier(Modifier::ITALIC)),
            Tag::Strong => self.push_style(self.current_style().add_modifier(Modifier::BOLD)),
            Tag::Strikethrough => {
                self.push_style(self.current_style().add_modifier(Modifier::CROSSED_OUT))
            }
            Tag::Link { dest_url, .. } => {
                self.push_style(link_style());
                self.link_url = Some(dest_url.into_string());
                self.link_text = Some(String::new());
            }
            Tag::Image { dest_url, .. } => {
                self.push_style(self.base.add_modifier(Modifier::DIM));
                self.image_url = Some(dest_url.into_string());
                self.image_alt = Some(String::new());
            }
            _ => {}
        }
    }

    fn end_tag(&mut self, end: TagEnd) {
        match end {
            TagEnd::Paragraph => {
                self.finalize_line();
                self.push_blank();
                self.containers.pop();
            }
            TagEnd::Heading(_) => {
                self.finalize_line();
                self.pop_prefix();
                self.pop_style();
                self.containers.pop();
            }
            TagEnd::BlockQuote(_) => {
                self.finalize_line();
                self.pop_prefix();
                self.containers.pop();
            }
            TagEnd::CodeBlock => {
                self.flush_code_block();
                self.in_code_block = false;
            }
            TagEnd::Item => {
                self.finalize_line();
                self.pop_prefix();
                self.containers.pop();
            }
            TagEnd::List(_) => {
                self.list_stack.pop();
            }
            TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough => {
                self.pop_style();
            }
            TagEnd::Link => {
                self.flush_link();
                self.pop_style();
            }
            TagEnd::Image => {
                self.flush_image();
                self.pop_style();
            }
            _ => {}
        }
    }

    fn text(&mut self, s: &str) {
        if self.in_code_block {
            self.code_buffer.push_str(s);
            return;
        }
        self.maybe_capture_link_or_alt(s);
        self.push_words(s, self.current_style());
    }

    fn code(&mut self, c: &str) {
        if self.in_code_block {
            self.code_buffer.push_str(c);
            return;
        }
        self.maybe_capture_link_or_alt(c);
        self.emit_word(c, code_style());
    }

    fn maybe_capture_link_or_alt(&mut self, s: &str) {
        if let Some(ref mut buf) = self.link_text {
            buf.push_str(s);
        }
        if let Some(ref mut buf) = self.image_alt {
            buf.push_str(s);
        }
    }

    fn soft_break(&mut self) {
        if !self.current.is_empty() {
            self.needs_space = true;
        }
    }

    fn hard_break(&mut self) {
        self.finalize_line();
        self.needs_space = false;
    }

    fn rule(&mut self) {
        self.finalize_line();
        let rule = "─".repeat(self.width.max(1));
        self.lines
            .push(Line::from(vec![Span::styled(rule, self.base)]));
    }

    fn task_marker(&mut self, checked: bool) {
        self.emit_word(if checked { "[x]" } else { "[ ]" }, self.base);
        self.needs_space = true;
    }

    fn push_words(&mut self, text: &str, style: Style) {
        for word in text.split_whitespace() {
            self.emit_word(word, style);
        }
    }

    fn emit_word(&mut self, word: &str, style: Style) {
        if word.is_empty() {
            return;
        }
        let word_width = word.chars().count();
        let prefix_width = self.first_prefix.chars().count();

        if self.current.is_empty() {
            let available = self.width.saturating_sub(prefix_width).max(1);
            if word_width > available {
                let (head, rest) = split_at_width(word, available);
                self.current.push(Span::styled(head.to_string(), style));
                self.current_width += head.chars().count();
                self.finalize_line();
                self.emit_word(rest, style);
                return;
            }
            self.current.push(Span::styled(word.to_string(), style));
            self.current_width = word_width;
            self.needs_space = true;
        } else {
            let space = if self.needs_space { 1 } else { 0 };
            if self.current_width + space + word_width > self.width {
                self.finalize_line();
                self.emit_word(word, style);
                return;
            }
            if self.needs_space {
                self.current.push(Span::styled(" ".to_string(), self.base));
                self.current_width += 1;
            }
            self.current.push(Span::styled(word.to_string(), style));
            self.current_width += word_width;
            self.needs_space = true;
        }
    }

    fn finalize_line(&mut self) {
        if self.current.is_empty() {
            return;
        }
        let prefix = self.first_prefix.clone();
        let mut spans = Vec::new();
        if !prefix.is_empty() {
            spans.push(Span::styled(prefix, self.base));
        }
        spans.append(&mut self.current);
        self.lines.push(Line::from(spans));
        self.current_width = 0;
        self.first_prefix = self.cont_prefix.clone();
        self.needs_space = false;
    }

    fn push_blank(&mut self) {
        if self
            .lines
            .last()
            .map(|l| l.spans.is_empty())
            .unwrap_or(false)
        {
            return;
        }
        self.lines.push(Line::from(""));
    }

    fn finish(mut self) -> Vec<Line<'static>> {
        self.finalize_line();
        self.lines
    }

    fn push_prefix(&mut self, first: &str, cont: &str) {
        self.prefix_stack
            .push((self.first_prefix.clone(), self.cont_prefix.clone()));
        self.first_prefix = first.to_string();
        self.cont_prefix = cont.to_string();
    }

    fn pop_prefix(&mut self) {
        if let Some((first, cont)) = self.prefix_stack.pop() {
            self.first_prefix = first;
            self.cont_prefix = cont;
        }
    }

    fn push_style(&mut self, style: Style) {
        self.style_stack.push(style);
    }

    fn pop_style(&mut self) {
        self.style_stack.pop();
    }

    fn flush_code_block(&mut self) {
        let prefix = Span::styled("  │ ", Style::default().fg(MUTED));
        let style = Style::default().fg(TEXT).bg(CODE_BG);
        for line in self.code_buffer.lines() {
            self.lines.push(Line::from(vec![
                prefix.clone(),
                Span::styled(line.to_string(), style),
            ]));
        }
        if !self.code_buffer.is_empty() && self.code_buffer.ends_with('\n') {
            // lines() already produced the content; trailing newline is fine.
        }
    }

    fn flush_link(&mut self) {
        let url = self.link_url.take().unwrap_or_default();
        let text = self.link_text.take().unwrap_or_default().trim().to_string();
        if !url.is_empty() && !text.is_empty() && text != url {
            self.emit_word(&format!("({})", url), Style::default().fg(MUTED));
        }
    }

    fn flush_image(&mut self) {
        let _url = self.image_url.take().unwrap_or_default();
        let alt = self.image_alt.take().unwrap_or_default().trim().to_string();
        if !alt.is_empty() {
            self.emit_word(&format!("🖼 {}", alt), self.base.add_modifier(Modifier::DIM));
        }
    }
}

fn heading_prefix(level: u8) -> String {
    let hashes = "#".repeat(level.clamp(1, 6) as usize);
    format!("{} ", hashes)
}

fn list_item_prefix(frames: &[ListFrame], number: usize) -> (String, String) {
    let depth = frames.len().saturating_sub(1);
    let indent = "  ".repeat(depth);
    if let Some(frame) = frames.last() {
        let bullet = if frame.ordered {
            format!("{}. ", number)
        } else {
            "• ".to_string()
        };
        let cont = " ".repeat(bullet.chars().count());
        (
            format!("{}{}", indent, bullet),
            format!("{}{}", indent, cont),
        )
    } else {
        (indent.clone(), indent)
    }
}

fn link_style() -> Style {
    Style::default().fg(LINK).add_modifier(Modifier::UNDERLINED)
}

fn code_style() -> Style {
    Style::default().fg(TEXT).bg(CODE_BG)
}

fn split_at_width(s: &str, width: usize) -> (&str, &str) {
    if width == 0 {
        return ("", s);
    }
    let mut bytes = 0;
    for (chars, c) in s.chars().enumerate() {
        if chars >= width {
            break;
        }
        bytes += c.len_utf8();
    }
    s.split_at(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line_text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.clone()).collect()
    }

    #[test]
    fn test_plain_paragraph_wraps_to_width() {
        let lines = markdown_lines("one two three four", 10);
        assert_eq!(line_text(&lines[0]), "one two");
        assert_eq!(line_text(&lines[1]), "three four");
    }

    #[test]
    fn test_bold_and_italic() {
        let lines = markdown_lines("**bold** and *italic*", 80);
        let text: String = lines.iter().map(line_text).collect();
        assert!(text.contains("bold"));
        assert!(text.contains("italic"));
        assert!(
            lines.len() <= 2,
            "bold/italic text should fit on one line plus optional trailing blank"
        );
    }

    #[test]
    fn test_link_renders_url_fallback() {
        let lines = markdown_lines("[text](https://example.com)", 80);
        let text: String = lines.iter().map(line_text).collect();
        assert!(text.contains("text"));
        assert!(text.contains("https://example.com"));
    }

    #[test]
    fn test_list_prefix_and_indent() {
        let lines = markdown_lines("- alpha\n- beta", 20);
        let text: String = lines.iter().map(|l| line_text(l) + "\n").collect();
        assert!(text.contains("• alpha"));
        assert!(text.contains("• beta"));
    }

    #[test]
    fn test_code_block_lines_are_prefixed() {
        let lines = markdown_lines("```\nhello\nworld\n```", 20);
        assert!(lines.iter().any(|l| line_text(l).starts_with("  │ ")));
    }
}
