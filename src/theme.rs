//! Color palette and semantic roles, lifted directly from the design system
//! reference frame (Frame 9) of the source mockup.

use ratatui::style::Color;

pub const BG_OUTER: Color = Color::Rgb(0x1e, 0x1f, 0x29);
pub const BG_PANEL: Color = Color::Rgb(0x28, 0x2a, 0x36);
pub const BG_SELECTION: Color = Color::Rgb(0x44, 0x47, 0x5a);
pub const FG: Color = Color::Rgb(0xf8, 0xf8, 0xf2);
pub const DIM: Color = Color::Rgb(0x62, 0x72, 0xa4);

pub const GREEN: Color = Color::Rgb(0x50, 0xfa, 0x7b); // added / clean
pub const RED: Color = Color::Rgb(0xff, 0x55, 0x55); // removed / flagged for rework
pub const PURPLE: Color = Color::Rgb(0xbd, 0x93, 0xf9); // hunk header
pub const PINK: Color = Color::Rgb(0xff, 0x79, 0xc6); // note to agent / agent-authored
pub const CYAN: Color = Color::Rgb(0x8b, 0xe9, 0xfd); // selected / cursor / human-edited
pub const ORANGE: Color = Color::Rgb(0xff, 0xb8, 0x6c); // has open notes / in-progress
pub const YELLOW: Color = Color::Rgb(0xf1, 0xfa, 0x8c); // syntax: string literals

/// Status of a file in the review/curation flow.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FileStatus {
    Clean,
    HasNotes,
    Flagged,
}

impl FileStatus {
    pub fn glyph(self) -> &'static str {
        match self {
            FileStatus::Clean => "\u{2714}",    // ✔
            FileStatus::HasNotes => "\u{29d6}", // ⧖
            FileStatus::Flagged => "\u{2717}",  // ✗
        }
    }

    pub fn color(self) -> Color {
        match self {
            FileStatus::Clean => GREEN,
            FileStatus::HasNotes => ORANGE,
            FileStatus::Flagged => RED,
        }
    }
}
