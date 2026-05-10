use chrono::{DateTime, Local, Utc};
use polyphony_core::InboxItemRow;

pub(crate) fn item_time_label(item: &InboxItemRow) -> String {
    item.created_at
        .map(format_relative_days)
        .unwrap_or_else(|| "—".into())
}

fn format_relative_days(dt: DateTime<Utc>) -> String {
    let now = Utc::now();
    let days = now.signed_duration_since(dt).num_days();
    if days <= 0 {
        let local: DateTime<Local> = dt.into();
        local.format("%H:%M").to_string()
    } else {
        format!("{days}d")
    }
}

pub(crate) fn truncate(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    let mut out = value
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncation_preserves_short_strings() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("abcdef", 4), "abc…");
    }
}
