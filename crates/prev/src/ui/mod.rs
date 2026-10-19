//! prev's interface: scramble-ui, the Material Design 3 components and
//! desktop integration shared by Scramble Tools apps, and prev's own
//! export dialog.

pub use scramble_ui::*;

pub mod export;

use iced::Element;

/// The open button a window's toolbar starts with.
pub fn open_file<'a, Message: Clone + 'a>(message: Message) -> Element<'a, Message> {
    component::tool(Icon::FolderOpen, crate::fl!("action-open"), Some(message))
}

/// The close button for a window without a toolbar, where the window
/// manager draws no title bar to close it from, such as on Hyprland or
/// niri.
pub fn close_window<'a, Message: Clone + 'a>(message: Message) -> Option<Element<'a, Message>> {
    (!desktop::draws_title_bars())
        .then(|| component::tool(Icon::Close, crate::fl!("menu-close"), Some(message)))
}

/// The full screen and close buttons a window's toolbar ends with where
/// the window manager draws no title bar to have them in.
pub fn window_buttons<'a, Message: Clone + 'a>(
    fullscreen: bool,
    toggle_fullscreen: Message,
    close: Message,
) -> Option<Element<'a, Message>> {
    let (glyph, label) = if fullscreen {
        (Icon::FullscreenExit, crate::fl!("menu-exit-full-screen"))
    } else {
        (Icon::Fullscreen, crate::fl!("menu-full-screen"))
    };
    Some(component::group([
        component::tool(glyph, label, Some(toggle_fullscreen)),
        close_window(close)?,
    ]))
}

/// The width of [`window_buttons`] in a toolbar.
pub const WINDOW_BUTTONS_WIDTH: f32 = component::TOOL_WIDTH * 2.0 + 4.0;
