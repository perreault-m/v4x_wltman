//! Shared color palette for the V4X Wallet Manager GUI.
//!
//! Centralizing these here -- rather than scattering `Color::from_rgb(...)`
//! calls across every view function -- is what keeps new screens visually
//! consistent "for free": a new panel built from `widgets::card` and these
//! constants looks right without anyone having to eyeball colors again.
//!
//! Author: Michael.P for V4X
//! Date: 2026-07-31

use iced::Color;

// --- "V4X" palette: technological green on a very dark background ---
pub const ACCENT: Color = Color::from_rgb(0.0, 0.95, 0.35);
pub const ACCENT_HOVER: Color = Color::from_rgb(0.25, 1.0, 0.55);
pub const ACCENT_PRESS: Color = Color::from_rgb(0.0, 0.65, 0.25);
pub const WARNING: Color = Color::from_rgb(1.0, 0.62, 0.0);
pub const WARNING_HOVER: Color = Color::from_rgb(1.0, 0.75, 0.25);
pub const SUCCESS: Color = Color::from_rgb(0.25, 0.95, 0.45);
pub const ERROR: Color = Color::from_rgb(1.0, 0.35, 0.35);
pub const MUTED: Color = Color::from_rgb(0.55, 0.68, 0.6);
/// Burnt orange used for panel titles, to distinguish them from the green
/// accent used elsewhere (addresses, active states, etc).
pub const TITLE_COLOR: Color = Color::from_rgb(0.80, 0.40, 0.12);
pub const PAGE_BG: Color = Color::from_rgb(0.02, 0.03, 0.025);
pub const PANEL_BG: Color = Color::from_rgb(0.05, 0.08, 0.06);
pub const PANEL_BORDER: Color = Color::from_rgba(0.0, 0.95, 0.35, 0.25);
