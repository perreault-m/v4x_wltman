//! Small internal "UI kit" for the V4X Wallet Manager GUI: a consistent,
//! reusable set of styled widgets (buttons, cards/panels, modal overlay,
//! labeled rows, tab bar) built on top of `iced`, plus the shared color
//! palette they're built from.
//!
//! This lives separately from `bin/gui.rs` on purpose: as the app grows
//! more tabs/screens, they can all pull from here (`ui::card`,
//! `ui::primary_button`, `ui::tab_bar`, ...) and stay visually consistent
//! without copy-pasting styling code or coupling this module to any one
//! screen's state. Nothing in here knows about `gui::Message`, `gui::Tab`,
//! wallets, or the XRPL -- it's pure `iced` presentation, generic over
//! whatever `Message` type the caller uses.
//!
//! Only included by the `gui` binary (`#[path = "../ui.rs"] mod ui;` in
//! `bin/gui.rs`) -- the CLI has no UI.
//!
//! Adding a new reusable widget: add it to `widgets.rs` (or a new
//! `ui/xxx.rs` submodule declared here the same way as `theme`/`widgets`
//! below, if it's a big enough chunk to deserve its own file) and it's
//! automatically available as `ui::xxx` everywhere via the re-export below.
//!
//! Note on the `#[path]` attributes below: since this file is itself
//! reached via an explicit `#[path]` from `bin/gui.rs` (rather than the
//! plain `mod ui;` convention), Rust does NOT automatically look for its
//! submodules in a `ui/` subdirectory the way it would for a normally
//! discovered module -- that convention only kicks in for modules found
//! through the default (non-`#[path]`) mechanism. So each submodule here
//! needs its own explicit `#[path]`, spelled out relative to this same
//! base directory.
//!
//! Author: Michael.P for V4X
//! Date: 2026-07-31

#[path = "ui/theme.rs"]
pub mod theme;
#[path = "ui/widgets.rs"]
pub mod widgets;

pub use theme::*;
pub use widgets::*;