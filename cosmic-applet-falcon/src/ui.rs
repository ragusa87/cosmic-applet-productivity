use cosmic::Element;
use cosmic::iced::{Color, Length};
use cosmic::widget::{Column, Row, button, text};

use crate::app::{AppModel, Message};
use crate::systemd::{ActiveState, Snapshot, UnitAction};

const GREEN: Color = Color::from_rgb(0.13, 0.65, 0.29);
const RED: Color = Color::from_rgb(0.85, 0.13, 0.16);
const AMBER: Color = Color::from_rgb(0.85, 0.55, 0.05);

fn state_color(state: ActiveState) -> Option<Color> {
    match state {
        ActiveState::Active | ActiveState::Reloading => Some(GREEN),
        ActiveState::Failed => Some(RED),
        ActiveState::Activating | ActiveState::Deactivating => Some(AMBER),
        ActiveState::Inactive | ActiveState::Unknown => None,
    }
}

pub fn popup_view(state: &AppModel) -> Element<'_, Message> {
    let mut col = Column::new()
        .padding(12)
        .spacing(8)
        .push(text::title4("Falcon Sensor"));

    if let Some(e) = &state.status_error {
        col = col.push(text::body(format!("Status unavailable: {e}")).class(RED));
    }

    if let Some(snapshot) = &state.snapshot {
        col = col.push(status_rows(snapshot));
    } else if state.status_error.is_none() {
        col = col.push(text::body("Reading service status\u{2026}"));
    }

    if let Some(e) = &state.action_error {
        col = col.push(text::body(e.clone()).class(RED));
    }

    if let Some(action) = state.pending {
        col = col.push(text::caption(action.progress_label()));
    }

    col.push(action_row(state)).into()
}

fn status_rows(snapshot: &Snapshot) -> Element<'_, Message> {
    let mut col = Column::new().spacing(4);

    let service_label = format!(
        "Service: {} ({})",
        snapshot.active_state.label(),
        snapshot.sub_state
    );
    let mut service = text::body(service_label);
    if let Some(color) = state_color(snapshot.active_state) {
        service = service.class(color);
    }
    col = col.push(service);

    let process_label = if snapshot.processes.is_empty() {
        "Process: not running".to_owned()
    } else {
        let list = snapshot
            .processes
            .iter()
            .map(|p| format!("{} (pid {})", p.comm, p.pid))
            .collect::<Vec<_>>()
            .join(", ");
        format!("Process: {list}")
    };
    col = col.push(text::caption(process_label));

    if let Some(file_state) = &snapshot.unit_file_state {
        col = col.push(text::caption(format!("Unit file: {file_state}")));
    }

    col.into()
}

fn action_row(state: &AppModel) -> Element<'_, Message> {
    let running = state.is_running();
    let idle = state.pending.is_none();

    let mut start = button::suggested("Start");
    if idle && !running {
        start = start.on_press(Message::RunAction(UnitAction::Start));
    }

    let mut stop = button::destructive("Stop");
    if idle && running {
        stop = stop.on_press(Message::RunAction(UnitAction::Stop));
    }

    let mut restart = button::standard("Restart");
    if idle && running {
        restart = restart.on_press(Message::RunAction(UnitAction::Restart));
    }

    Row::new()
        .spacing(8)
        .width(Length::Fill)
        .push(start)
        .push(stop)
        .push(restart)
        .into()
}
