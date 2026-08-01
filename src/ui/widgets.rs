//! Reusable, styled `iced` widgets shared across the app's screens/tabs.
//!
//! Everything here is generic over the caller's `Message` type: this module
//! knows nothing about `gui::Message`, `gui::Tab`, or any app state. That's
//! deliberate -- it's what lets a brand-new tab reuse `card`,
//! `primary_button`, `modal`, `tab_bar`, etc. and automatically look
//! consistent with the rest of the app, without this module ever needing to
//! change as the UI grows. App-specific screens stay in `bin/gui.rs` (or
//! their own files); only genuinely cross-cutting widgets belong here.
//!
//! Author: Michael.P for V4X
//! Date: 2026-07-31

use iced::widget::{
    button, center, column, container, mouse_area, opaque, row, scrollable, stack, text,
};
use iced::{Background, Border, Color, Element, Length, Theme};

use super::theme::{ACCENT, MUTED, PANEL_BG, PANEL_BORDER, TITLE_COLOR, WARNING, WARNING_HOVER};

// ============================== Panels ("cards") ==============================

/// Common style for panels ("cards"): slightly greenish dark background,
/// subtle green border, rounded corners.
pub fn card_style(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(PANEL_BG)),
        border: Border {
            color: PANEL_BORDER,
            width: 1.0,
            radius: 10.0.into(),
        },
        ..container::Style::default()
    }
}

/// Wraps `content` in a titled panel ("card") of the given width. Takes an
/// owned title (rather than `&str`) so callers can pass the result of a
/// translation lookup (`t(...)`) directly, without needing a separate local
/// binding to keep a temporary alive.
pub fn card<'a, Message: 'a>(
    title: impl Into<String>,
    content: Element<'a, Message>,
    width: Length,
) -> Element<'a, Message> {
    container(column![text(title.into()).size(13).color(TITLE_COLOR), content].spacing(16))
        .padding(20)
        .width(width)
        .style(card_style)
        .into()
}

// ============================== Buttons ==============================

/// Solid, high-emphasis button (green accent). Use for the primary action
/// on a screen (generate, load, review, send on testnet, ...).
pub fn primary_button(_theme: &Theme, status: button::Status) -> button::Style {
    let background = match status {
        button::Status::Hovered => super::theme::ACCENT_HOVER,
        button::Status::Pressed => super::theme::ACCENT_PRESS,
        button::Status::Disabled => Color { a: 0.3, ..ACCENT },
        button::Status::Active => ACCENT,
    };

    button::Style {
        background: Some(Background::Color(background)),
        text_color: Color::BLACK,
        border: Border {
            radius: 8.0.into(),
            width: 0.0,
            color: Color::TRANSPARENT,
        },
        ..button::Style::default()
    }
}

/// Warning variant of the primary button (orange background) -- use for a
/// primary action that carries real-world risk (e.g. sending on mainnet).
pub fn warning_button(_theme: &Theme, status: button::Status) -> button::Style {
    let background = match status {
        button::Status::Hovered => WARNING_HOVER,
        button::Status::Pressed => WARNING,
        button::Status::Disabled => Color { a: 0.3, ..WARNING },
        button::Status::Active => WARNING,
    };

    button::Style {
        background: Some(Background::Color(background)),
        text_color: Color::BLACK,
        border: Border {
            radius: 8.0.into(),
            width: 0.0,
            color: Color::TRANSPARENT,
        },
        ..button::Style::default()
    }
}

/// Outlined, low-emphasis button. Use for secondary/cancel actions.
pub fn secondary_button(_theme: &Theme, status: button::Status) -> button::Style {
    let (border_color, text_color, fill_alpha) = match status {
        button::Status::Hovered => (ACCENT, ACCENT, 0.1),
        button::Status::Pressed => (ACCENT, ACCENT, 0.18),
        button::Status::Disabled => (Color { a: 0.3, ..ACCENT }, Color { a: 0.3, ..ACCENT }, 0.0),
        button::Status::Active => (ACCENT, ACCENT, 0.0),
    };

    button::Style {
        background: Some(Background::Color(Color {
            a: fill_alpha,
            ..ACCENT
        })),
        text_color,
        border: Border {
            radius: 8.0.into(),
            width: 1.5,
            color: border_color,
        },
        ..button::Style::default()
    }
}

// ============================== Modal overlay ==============================

/// Overlays `content` on top of `base` with a near-opaque background
/// (clicking outside the content sends `on_blur`, typically to close a
/// modal).
pub fn modal<'a, Message: Clone + 'a>(
    base: Element<'a, Message>,
    content: Element<'a, Message>,
    on_blur: Message,
) -> Element<'a, Message> {
    stack![
        base,
        opaque(
            mouse_area(center(opaque(content)).style(|_theme| container::Style {
                background: Some(Background::Color(Color { a: 0.92, ..Color::BLACK })),
                ..container::Style::default()
            }))
            .on_press(on_blur)
        )
    ]
    .into()
}

// ============================== Labeled rows ==============================

/// Renders a labeled, scrollable value row (e.g. "ADDRESS" over the address
/// value) -- used anywhere a long value like an address or tx hash needs a
/// label above it without wrapping/overflowing the panel.
pub fn info_row<'a, Message: 'a>(label: impl Into<String>, value: &'a str) -> Element<'a, Message> {
    column![
        text(label.into().to_uppercase()).size(11).color(MUTED),
        scrollable(text(value).size(14).color(ACCENT)).width(Length::Fill),
    ]
    .spacing(4)
    .into()
}

/// Variant of [`info_row`] for a locally computed value (e.g. `format!(...)`)
/// -- takes an owned `String` rather than a reference, to avoid any lifetime
/// issue with a temporary value.
pub fn owned_info_row<Message: 'static>(
    label: impl Into<String>,
    value: String,
) -> Element<'static, Message> {
    column![
        text(label.into().to_uppercase()).size(11).color(MUTED),
        scrollable(text(value).size(14).color(ACCENT)).width(Length::Fill),
    ]
    .spacing(4)
    .into()
}

// ============================== Tab bar ==============================

/// Row of tab-selector buttons, following the same primary/secondary
/// styling convention used everywhere else. Generic over both the tab
/// identifier type `T` and the caller's `Message` type, so any screen with
/// its own set of tabs can reuse this rather than hand-rolling one.
///
/// `tabs` is `(identifier, label)` pairs -- already localized/labeled by the
/// caller, since this module doesn't know about translation. `active` is
/// the currently selected identifier. `on_select` maps a clicked tab's
/// identifier to a `Message`.
pub fn tab_bar<'a, T, Message>(
    tabs: impl IntoIterator<Item = (T, String)>,
    active: T,
    on_select: impl Fn(T) -> Message + 'a,
) -> Element<'a, Message>
where
    T: PartialEq + Copy + 'a,
    Message: Clone + 'a,
{
    let buttons: Vec<Element<'a, Message>> = tabs
        .into_iter()
        .map(|(id, label)| {
            let is_active = id == active;
            button(text(label).size(14))
                .padding([10, 20])
                .style(move |theme: &Theme, status| {
                    if is_active {
                        primary_button(theme, status)
                    } else {
                        secondary_button(theme, status)
                    }
                })
                .on_press(on_select(id))
                .into()
        })
        .collect();

    row(buttons).spacing(10).into()
}