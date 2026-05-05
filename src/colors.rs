// Brand colors — synced with repos/web + repos/mobile + repos/site
//
// All terminal output should use these constants rather than raw `truecolor()`
// calls so color changes propagate everywhere from one place.
//
// Usage: `"text".custom_color(colors::AMBER)`
// Requires `use colored::Colorize;` to be in scope (already the case in
// every module that currently calls `.truecolor()`).

use colored::CustomColor;

// ── Primary brand ─────────────────────────────────────────────────────────────

/// Brand amber — encryption state indicator, primary actions, highlights.
pub const AMBER: CustomColor = CustomColor { r: 245, g: 184, b: 0 };

/// Slightly darker amber — used for tray/status icons.
pub const AMBER_DARK: CustomColor = CustomColor { r: 212, g: 168, b: 67 };

// ── Status ────────────────────────────────────────────────────────────────────

/// Success / healthy state.
pub const GREEN_OK: CustomColor = CustomColor { r: 143, g: 193, b: 139 };

/// Error / warning / destructive.
pub const RED_ERR: CustomColor = CustomColor { r: 224, g: 122, b: 106 };

// ── Text hierarchy ────────────────────────────────────────────────────────────

/// Primary output text — filenames, values, identifiers.
pub const INK: CustomColor = CustomColor { r: 233, g: 230, b: 221 };

/// Warm secondary text — labels, mount points, section headers.
pub const INK_WARM: CustomColor = CustomColor { r: 208, g: 200, b: 154 };

/// Dimmed / muted text — keys, metadata, hints.
pub const INK_DIM: CustomColor = CustomColor { r: 106, g: 101, b: 91 };

/// Sage dimmed text — footer hints, doc comments.
pub const INK_SAGE: CustomColor = CustomColor { r: 125, g: 138, b: 106 };
