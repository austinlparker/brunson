use ratatui::style::{Color, Modifier, Style};

use crate::github::types::PrGroup;

// ── Catppuccin Mocha palette ────────────────────────────────────────────────

// Base/grayscale
pub const BASE: Color = Color::Rgb(17, 17, 27); // #11111b
pub const MANTLE: Color = Color::Rgb(24, 24, 37); // #181825
pub const CRUST: Color = Color::Rgb(17, 17, 27); // #11111b
pub const SURFACE0: Color = Color::Rgb(49, 50, 68); // #313244
pub const SURFACE1: Color = Color::Rgb(69, 71, 90); // #45475a
pub const SURFACE2: Color = Color::Rgb(88, 91, 112); // #585b70
pub const OVERLAY0: Color = Color::Rgb(108, 112, 134); // #6c7086
pub const OVERLAY1: Color = Color::Rgb(127, 132, 156); // #7f849c
pub const SUBTEXT0: Color = Color::Rgb(166, 173, 200); // #a6adc8
pub const SUBTEXT1: Color = Color::Rgb(186, 194, 222); // #bac2de
pub const TEXT: Color = Color::Rgb(205, 214, 244); // #cdd6f4

// Semantic colors
pub const ADD: Color = Color::Rgb(166, 227, 161); // #a6e3a1
pub const ADD_BG: Color = Color::Rgb(26, 46, 34); // faint green bg
pub const DEL: Color = Color::Rgb(243, 139, 168); // #f38ba8
pub const DEL_BG: Color = Color::Rgb(48, 24, 32); // faint red bg
pub const HUNK: Color = Color::Rgb(116, 199, 236); // #74c7ec
pub const OPEN: Color = Color::Rgb(166, 227, 161); // #a6e3a1
pub const DRAFT: Color = Color::Rgb(127, 132, 156); // #7f849c
pub const REVIEW_REQUESTED: Color = Color::Rgb(137, 180, 250); // #89b4fa
pub const PASS: Color = Color::Rgb(166, 227, 161); // #a6e3a1
pub const FAIL: Color = Color::Rgb(243, 139, 168); // #f38ba8
pub const PENDING: Color = Color::Rgb(249, 226, 175); // #f9e2af

// Blade accents
pub const INBOX: Color = Color::Rgb(137, 180, 250); // #89b4fa blue
pub const OVERVIEW: Color = Color::Rgb(250, 179, 135); // #fab387 peach
pub const ACTIVITY: Color = Color::Rgb(203, 166, 247); // #cba6f7 mauve
pub const FILES: Color = Color::Rgb(166, 227, 161); // #a6e3a1 green
pub const DIFF: Color = Color::Rgb(148, 226, 213); // #94e2d5 teal

// Inline formatting
pub const LINK: Color = Color::Rgb(116, 199, 236); // #74c7ec sapphire
pub const CODE_BG: Color = Color::Rgb(69, 71, 90); // #45475a surface1

// Priority
pub const HIGH: Color = Color::Rgb(243, 139, 168); // #f38ba8
pub const MED: Color = Color::Rgb(250, 179, 135); // #fab387
pub const LOW: Color = Color::Rgb(108, 112, 134); // #6c7086

// Backwards-compatible semantic aliases for migration-era components.
pub const ACCENT: Color = INBOX;
pub const BORDER: Color = SURFACE1;
pub const PANEL_BG: Color = MANTLE;
pub const SELECT_BG: Color = SURFACE0;
pub const MUTED: Color = OVERLAY0;
pub const SUBTLE: Color = SUBTEXT0;
pub const GROUP: Color = OVERVIEW;

// ── Nerd Font icon glyphs (Codicons, present in MesloLGL Nerd Font) ─────────
pub const ICON_PR: &str = "\u{EA64}";
pub const ICON_PR_CLOSED: &str = "\u{EBDA}";
pub const ICON_PR_DRAFT: &str = "\u{EBDB}";
pub const ICON_COMMENT: &str = "\u{EA6B}";
pub const ICON_COMMENT_DISCUSSION: &str = "\u{EAC7}";
pub const ICON_CHECK: &str = "\u{EAB2}";
pub const ICON_CLOSE: &str = "\u{EA76}";
pub const ICON_ERROR: &str = "\u{EA87}";
pub const ICON_WARNING: &str = "\u{EA6C}";
pub const ICON_EYE: &str = "\u{EA70}";
pub const ICON_PERSON_ADD: &str = "\u{EBCD}";
pub const ICON_COMMIT: &str = "\u{EAFC}";
pub const ICON_MERGE: &str = "\u{EAFE}";
pub const ICON_REOPEN: &str = "\u{EB0B}";
pub const ICON_REQUEST_CHANGES: &str = "\u{EB43}";
pub const ICON_SYNC: &str = "\u{EA77}";
pub const ICON_CIRCLE_SLASH: &str = "\u{EABD}";
pub const ICON_DASH: &str = "\u{EACC}";
pub const ICON_FILE: &str = "\u{EA7B}";
pub const ICON_FILES: &str = "\u{EAF0}";
pub const ICON_INBOX: &str = "\u{EB09}";
pub const ICON_OVERVIEW: &str = "\u{EA7F}";
pub const ICON_ACTIVITY: &str = "\u{EB42}";
pub const ICON_DIFF: &str = "\u{EAE1}";
pub const ICON_DIFF_ADDED: &str = "\u{EADC}";
pub const ICON_DIFF_REMOVED: &str = "\u{EADF}";
pub const ICON_DIFF_MODIFIED: &str = "\u{EADE}";
pub const ICON_DIFF_RENAMED: &str = "\u{EAE0}";
pub const ICON_LINK: &str = "\u{EA7C}";
pub const ICON_FORCE_PUSH: &str = "\u{EB3F}";
pub const ICON_ROCKET: &str = "\u{EB44}";

pub mod icon {
    pub use super::{
        ICON_ACTIVITY as ACTIVITY, ICON_CHECK as CHECK, ICON_CIRCLE_SLASH as CIRCLE_SLASH,
        ICON_CLOSE as CLOSE, ICON_COMMENT as COMMENT,
        ICON_COMMENT_DISCUSSION as COMMENT_DISCUSSION, ICON_COMMIT as COMMIT, ICON_DIFF as DIFF,
        ICON_DIFF_ADDED as DIFF_ADDED, ICON_DIFF_MODIFIED as DIFF_MODIFIED,
        ICON_DIFF_REMOVED as DIFF_REMOVED, ICON_DIFF_RENAMED as DIFF_RENAMED, ICON_ERROR as ERROR,
        ICON_EYE as EYE, ICON_FILE as FILE, ICON_FILES as FILES, ICON_FORCE_PUSH as FORCE_PUSH,
        ICON_INBOX as INBOX, ICON_LINK as LINK, ICON_MERGE as MERGE, ICON_OVERVIEW as OVERVIEW,
        ICON_PERSON_ADD as PERSON_ADD, ICON_PR as PR, ICON_PR_CLOSED as PR_CLOSED,
        ICON_PR_DRAFT as PR_DRAFT, ICON_REOPEN as REOPEN, ICON_REQUEST_CHANGES as REQUEST_CHANGES,
        ICON_ROCKET as ROCKET, ICON_SYNC as SYNC, ICON_WARNING as WARNING,
    };
}

/// One of the five horizontal Brunson blades.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Blade {
    #[default]
    Inbox,
    Overview,
    Activity,
    Files,
    Diff,
}

impl Blade {
    pub const fn index(self) -> usize {
        match self {
            Blade::Inbox => 0,
            Blade::Overview => 1,
            Blade::Activity => 2,
            Blade::Files => 3,
            Blade::Diff => 4,
        }
    }

    pub const fn from_index(i: usize) -> Self {
        match i {
            0 => Blade::Inbox,
            1 => Blade::Overview,
            2 => Blade::Activity,
            3 => Blade::Files,
            _ => Blade::Diff,
        }
    }

    pub const fn accent(self) -> Color {
        match self {
            Blade::Inbox => INBOX,
            Blade::Overview => OVERVIEW,
            Blade::Activity => ACTIVITY,
            Blade::Files => FILES,
            Blade::Diff => DIFF,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Blade::Inbox => "inbox",
            Blade::Overview => "overview",
            Blade::Activity => "activity",
            Blade::Files => "files",
            Blade::Diff => "diff",
        }
    }

    pub const fn count() -> usize {
        5
    }
}

/// Semantic theme used by all new components.
#[derive(Debug, Clone, Copy, Default)]
pub struct Theme;

impl Theme {
    pub fn base_bg(&self) -> Style {
        Style::default().bg(BASE)
    }

    pub fn blade_bg(&self) -> Style {
        Style::default().bg(MANTLE)
    }

    pub fn selected(&self) -> Style {
        Style::default()
            .fg(TEXT)
            .bg(SURFACE0)
            .add_modifier(Modifier::BOLD)
    }

    pub fn unselected(&self) -> Style {
        Style::default().fg(TEXT).bg(MANTLE)
    }

    pub fn muted(&self) -> Style {
        Style::default().fg(OVERLAY0)
    }

    pub fn text(&self) -> Style {
        Style::default().fg(TEXT)
    }

    pub fn bold(&self, fg: Color) -> Style {
        Style::default().fg(fg).add_modifier(Modifier::BOLD)
    }

    pub fn dim(&self) -> Style {
        Style::default().fg(SUBTEXT0).add_modifier(Modifier::DIM)
    }

    pub fn accent(&self, blade: Blade) -> Style {
        Style::default().fg(blade.accent())
    }
}

/// Map a PR group to a display color.
pub fn state_color(group: PrGroup) -> Color {
    match group {
        PrGroup::ReviewNeeded | PrGroup::ReviewUpdate | PrGroup::ReviewDone => OPEN,
        PrGroup::Draft => DRAFT,
        PrGroup::AuthoredActionNeeded | PrGroup::AuthoredReadyToMerge | PrGroup::AuthoredWaiting => {
            OPEN
        }
        PrGroup::Other => REVIEW_REQUESTED,
    }
}
