//! prev's interface: scramble-ui, the Material Design 3 components and
//! desktop integration shared by Scramble Tools apps, and prev's own
//! export dialog.

pub use scramble_ui::*;

pub mod export;

use iced::Element;

/// The close button a window's toolbar ends with where the window manager
/// draws no title bar to close it from, such as on Hyprland or niri.
pub fn close_window<'a, Message: Clone + 'a>(message: Message) -> Option<Element<'a, Message>> {
    (!desktop::draws_title_bars())
        .then(|| component::tool(Icon::Close, crate::fl!("menu-close"), Some(message)))
}
