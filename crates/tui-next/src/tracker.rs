use polyphony_core::{RuntimeSnapshot, TrackerConnectionState};

pub(crate) fn label(snapshot: &RuntimeSnapshot) -> String {
    let tracker = snapshot.tracker_kind.to_string();
    let Some(connection) = snapshot.tracker_connection.as_ref() else {
        return tracker;
    };

    match connection.state {
        TrackerConnectionState::Connected => connection
            .label
            .as_deref()
            .filter(|label| !label.is_empty())
            .map_or(tracker.clone(), |label| format!("{tracker}:{label}")),
        TrackerConnectionState::Disconnected => format!(
            "{}:{}",
            tracker,
            connection.detail.as_deref().unwrap_or("disconnected")
        ),
        TrackerConnectionState::Unknown => format!("{tracker}:checking"),
    }
}
