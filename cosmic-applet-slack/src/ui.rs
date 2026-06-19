use cosmic::Element;
use cosmic::applet::menu_button;
use cosmic::widget::{Column, text};

use crate::app::Message;

pub fn menu_view<'a>(
    manual_paused: bool,
    effective_paused: bool,
    disable_during_weekend: bool,
) -> Element<'a, Message> {
    let pause_label = if manual_paused { "Resume" } else { "Pause" };
    let weekend_label = if disable_during_weekend {
        "Don't pause on weekends"
    } else {
        "Pause on weekends"
    };
    let mut col = Column::new()
        .padding(4)
        .spacing(0)
        .push(menu_button(text::body(pause_label)).on_press(Message::TogglePause))
        .push(menu_button(text::body(weekend_label)).on_press(Message::ToggleDisableDuringWeekend));
    if !effective_paused {
        col = col.push(menu_button(text::body("Refresh")).on_press(Message::RefreshFromMenu));
    }
    col.into()
}
