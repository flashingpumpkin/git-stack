//! Single helper for "now, as an RFC 3339 string". Centralised so tests
//! can mock by overriding `GIT_STACK_NOW` if we ever need to.

use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

pub fn now_rfc3339() -> String {
    if let Ok(s) = std::env::var("GIT_STACK_NOW") {
        return s;
    }
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| String::new())
}

/// Render an RFC 3339 timestamp as a human-friendly relative age, e.g.
/// "12 seconds ago", "3 minutes ago", "2 hours ago", "5 days ago". Falls
/// back to the raw string if parsing fails.
pub fn humanise_age(rfc3339: &str) -> String {
    let Ok(then) = OffsetDateTime::parse(rfc3339, &Rfc3339) else {
        return rfc3339.to_string();
    };
    let now = if let Ok(s) = std::env::var("GIT_STACK_NOW") {
        OffsetDateTime::parse(&s, &Rfc3339).unwrap_or_else(|_| OffsetDateTime::now_utc())
    } else {
        OffsetDateTime::now_utc()
    };
    let delta = now - then;
    let secs = delta.whole_seconds().max(0);
    if secs < 60 {
        return format!("{secs}s ago");
    }
    let mins = secs / 60;
    if mins < 60 {
        return format!("{mins}m ago");
    }
    let hours = mins / 60;
    if hours < 24 {
        return format!("{hours}h ago");
    }
    let days = hours / 24;
    format!("{days}d ago")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn humanises_relative_to_overridden_now() {
        // Pin now to a known instant.
        std::env::set_var("GIT_STACK_NOW", "2026-05-12T12:00:00Z");

        assert_eq!(humanise_age("2026-05-12T11:59:30Z"), "30s ago");
        assert_eq!(humanise_age("2026-05-12T11:55:00Z"), "5m ago");
        assert_eq!(humanise_age("2026-05-12T09:00:00Z"), "3h ago");
        assert_eq!(humanise_age("2026-05-10T12:00:00Z"), "2d ago");
        // Unparseable input falls back to the raw string.
        assert_eq!(humanise_age("not-a-date"), "not-a-date");

        std::env::remove_var("GIT_STACK_NOW");
    }
}
