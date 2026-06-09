use cosmic::Element;
use cosmic::applet::menu_button;
use cosmic::widget::{Column, text};

use crate::app::Message;

pub fn menu_view<'a>() -> Element<'a, Message> {
    Column::new()
        .padding(4)
        .spacing(0)
        .push(menu_button(text::body("Refresh")).on_press(Message::RefreshFromMenu))
        .into()
}
