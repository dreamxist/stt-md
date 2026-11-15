//! What the cloud summarizer costs: one line per call in `usage.jsonl`, and a
//! monthly total for the menu.
//!
//! The cost is computed when the call happens and stored with it, so a price
//! change later doesn't rewrite what was already spent.

use chrono::{DateTime, Datelike, Local, NaiveDate};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::Path;

#[derive(Debug, Serialize, Deserialize)]
pub struct UsageEntry {
    pub at: DateTime<Local>,
    pub model: String,
    pub input_tokens: u64,
    /// Answer plus thinking: Gemini bills both as output.
    pub output_tokens: u64,
    /// `None` when the model has no known price.
    pub cost_usd: Option<f64>,
}

/// USD per million tokens (input, output) on the standard paid tier, from
/// ai.google.dev/gemini-api/docs/pricing as of October 2026.
pub fn price_per_million(model: &str, on: NaiveDate) -> Option<(f64, f64)> {
    let from_2027 = on >= NaiveDate::from_ymd_opt(2027, 1, 1)?;
    match model {
        "gemini-3.8-flash" if from_2027 => Some((1.50, 7.50)),
        "gemini-3.8-flash" => Some((0.75, 3.75)),
        _ => None,
    }
}

pub fn entry(model: &str, input_tokens: u64, output_tokens: u64, at: DateTime<Local>) -> UsageEntry {
    let cost_usd = price_per_million(model, at.date_naive()).map(|(i, o)| {
        (input_tokens as f64 * i + output_tokens as f64 * o) / 1_000_000.0
    });
    UsageEntry { at, model: model.to_string(), input_tokens, output_tokens, cost_usd }
}

fn ledger() -> std::path::PathBuf {
    crate::paths::app_support_dir().join("usage.jsonl")
}

/// Appends one call to the ledger. Never fails the summary over bookkeeping.
pub fn record(model: &str, input_tokens: u64, output_tokens: u64) {
    let e = entry(model, input_tokens, output_tokens, Local::now());
    println!(
        "[stt-md] {model}: {input_tokens} in / {output_tokens} out tokens, US${:.4}",
        e.cost_usd.unwrap_or(0.0)
    );
    let line = match serde_json::to_string(&e) {
        Ok(l) => l,
        Err(_) => return,
    };
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(ledger())
        .and_then(|mut f| writeln!(f, "{line}"));
}

#[derive(Debug, Default, PartialEq)]
pub struct MonthTotal {
    pub calls: usize,
    pub cost_usd: f64,
}

pub fn month_total(on: NaiveDate) -> MonthTotal {
    total_in(&ledger(), on)
}

fn total_in(path: &Path, on: NaiveDate) -> MonthTotal {
    let Ok(text) = std::fs::read_to_string(path) else {
        return MonthTotal::default();
    };
    text.lines()
        .filter_map(|l| serde_json::from_str::<UsageEntry>(l).ok())
        .filter(|e| e.at.year() == on.year() && e.at.month() == on.month())
        .fold(MonthTotal::default(), |mut t, e| {
            t.calls += 1;
            t.cost_usd += e.cost_usd.unwrap_or(0.0);
            t
        })
}

/// "oct" for the menu label.
pub fn month_short_es(on: NaiveDate) -> &'static str {
    ["ene", "feb", "mar", "abr", "may", "jun", "jul", "ago", "sep", "oct", "nov", "dic"]
        [on.month0() as usize]
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(y: i32, m: u32, d: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(y, m, d, 12, 0, 0).unwrap()
    }

    #[test]
    fn cost_uses_the_price_of_the_day_of_the_call() {
        let oct = entry("gemini-3.8-flash", 20_000, 3_000, at(2026, 10, 20));
        assert!((oct.cost_usd.unwrap() - 0.02625).abs() < 1e-9);
        let jan = entry("gemini-3.8-flash", 20_000, 3_000, at(2027, 1, 2));
        assert!((jan.cost_usd.unwrap() - 0.0525).abs() < 1e-9);
        assert_eq!(entry("gemini-unknown", 1, 1, at(2026, 10, 20)).cost_usd, None);
    }

    #[test]
    fn month_total_only_counts_that_month() {
        let dir = std::env::temp_dir().join(format!("stt-md-usage-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("usage.jsonl");
        let lines: Vec<String> = [
            entry("gemini-3.8-flash", 20_000, 3_000, at(2026, 10, 1)),
            entry("gemini-3.8-flash", 20_000, 3_000, at(2026, 10, 20)),
            entry("gemini-3.8-flash", 20_000, 3_000, at(2026, 9, 30)),
        ]
        .iter()
        .map(|e| serde_json::to_string(e).unwrap())
        .chain(std::iter::once("not json".to_string()))
        .collect();
        std::fs::write(&path, lines.join("\n")).unwrap();
        let t = total_in(&path, at(2026, 10, 25).date_naive());
        assert_eq!(t.calls, 2);
        assert!((t.cost_usd - 0.0525).abs() < 1e-9);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
