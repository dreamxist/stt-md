pub mod daily_appender;
pub mod meeting_writer;
pub mod scanner;
pub mod simple_writer;

use chrono::{DateTime, Datelike, Local};

/// Expands `{year}`, `{month}`, `{week}` (ISO, 2 dígitos) and `{date}`
/// placeholders in a vault-relative path pattern.
pub fn expand_pattern(pattern: &str, dt: &DateTime<Local>) -> String {
    pattern
        .replace("{year}", &dt.format("%Y").to_string())
        .replace("{month}", &dt.format("%m").to_string())
        .replace("{week}", &format!("{:02}", dt.iso_week().week()))
        .replace("{date}", &dt.format("%Y-%m-%d").to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn expand_pattern_replaces_all_placeholders() {
        // 2026-04-27 fue lunes de la semana ISO 18.
        let dt = Local.with_ymd_and_hms(2026, 4, 27, 10, 0, 0).unwrap();
        assert_eq!(
            expand_pattern("journey/{year}/{month}/semana-{week}/meetings", &dt),
            "journey/2026/04/semana-18/meetings"
        );
        assert_eq!(
            expand_pattern("2-calendar/{year}/{month}/{date}.md", &dt),
            "2-calendar/2026/04/2026-04-27.md"
        );
        assert_eq!(expand_pattern("sin-placeholders", &dt), "sin-placeholders");
    }
}
