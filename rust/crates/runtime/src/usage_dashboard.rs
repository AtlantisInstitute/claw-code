use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io;
use std::path::{Path, PathBuf};

use crate::session::Session;
use crate::usage::UsageTracker;

/// Per-session usage snapshot for cross-session aggregation.
#[derive(Debug, Clone)]
pub struct UsageSnapshot {
    pub session_id: String,
    pub created_at_ms: u64,
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_usd: f64,
    pub turn_count: usize,
}

/// Scan session JSONL files in the given directory and extract usage snapshots.
///
/// Files that fail to parse are silently skipped.  Results are returned sorted
/// by `created_at_ms` descending (most recent first).
#[must_use]
pub fn scan_session_files(sessions_dir: &Path) -> Vec<UsageSnapshot> {
    let Ok(entries) = std::fs::read_dir(sessions_dir) else {
        return Vec::new();
    };

    let mut snapshots = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let is_session = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|ext| ext == "jsonl" || ext == "json");
        if !is_session {
            continue;
        }
        if let Some(snapshot) = snapshot_from_session_file(&path) {
            snapshots.push(snapshot);
        }
    }
    snapshots.sort_by(|a, b| b.created_at_ms.cmp(&a.created_at_ms));
    snapshots
}

/// Try to load a session file and extract a usage snapshot from it.
fn snapshot_from_session_file(path: &Path) -> Option<UsageSnapshot> {
    let session = Session::load_from_path(path).ok()?;
    let tracker = UsageTracker::from_session(&session);
    let cumulative = tracker.cumulative_usage();

    // Skip sessions with zero usage — nothing useful to report.
    if cumulative.total_tokens() == 0 {
        return None;
    }

    // The Session struct does not store per-message model info, so we fall
    // back to "unknown".  The tracker gives us aggregate token counts which
    // is the primary value here.
    let model = String::from("unknown");

    // Cost estimate using the default (sonnet-tier) pricing.
    let cost = cumulative.estimate_cost_usd();

    Some(UsageSnapshot {
        session_id: session.session_id.clone(),
        created_at_ms: session.created_at_ms,
        model,
        input_tokens: u64::from(cumulative.input_tokens),
        output_tokens: u64::from(cumulative.output_tokens),
        cost_usd: cost.total_cost_usd(),
        turn_count: tracker.turns() as usize,
    })
}

/// Aggregate total costs grouped by date (`YYYY-MM-DD`).
#[must_use]
pub fn aggregate_daily_costs(snapshots: &[UsageSnapshot]) -> BTreeMap<String, f64> {
    let mut daily = BTreeMap::new();
    for s in snapshots {
        let date = format_date_from_ms(s.created_at_ms);
        *daily.entry(date).or_insert(0.0) += s.cost_usd;
    }
    daily
}

/// Aggregate token totals and costs grouped by model name.
///
/// Returns `(input_tokens, output_tokens, cost_usd)` per model.
#[must_use]
pub fn aggregate_by_model(snapshots: &[UsageSnapshot]) -> BTreeMap<String, (u64, u64, f64)> {
    let mut models: BTreeMap<String, (u64, u64, f64)> = BTreeMap::new();
    for s in snapshots {
        let entry = models
            .entry(s.model.clone())
            .or_insert((0u64, 0u64, 0.0f64));
        entry.0 += s.input_tokens;
        entry.1 += s.output_tokens;
        entry.2 += s.cost_usd;
    }
    models
}

/// Convert epoch milliseconds to a `YYYY-MM-DD` date string (UTC).
///
/// Uses a pure-arithmetic civil-date calculation to avoid pulling in a
/// datetime dependency.
#[must_use]
pub fn format_date_from_ms(ms: u64) -> String {
    #[allow(clippy::cast_possible_wrap)]
    let days_since_epoch = (ms / 1000 / 86400) as i64;
    let (year, month, day) = civil_from_days(days_since_epoch);
    format!("{year:04}-{month:02}-{day:02}")
}

/// Civil date from days since 1970-01-01 (Howard Hinnant algorithm).
///
/// The casts here are safe for any reasonable epoch day count (years 0..9999).
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_possible_wrap)]
fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u32; // day of era [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // year of era [0, 399]
    let y = (i64::from(yoe) + era * 400) as i32;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // day of year [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

/// Format a token count for human display (e.g. `1.2M`, `45.3K`, `800`).
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn format_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

/// Generate a self-contained HTML usage report from the given snapshots.
#[must_use]
pub fn generate_html_report(snapshots: &[UsageSnapshot]) -> String {
    let daily = aggregate_daily_costs(snapshots);
    let by_model = aggregate_by_model(snapshots);
    // The `+ 0.0` canonicalizes IEEE 754 negative zero to positive zero so
    // the formatted output never shows "-$0.00" for an empty snapshot list.
    let total_cost: f64 = snapshots.iter().map(|s| s.cost_usd).sum::<f64>() + 0.0;
    let total_sessions = snapshots.len();
    let total_input: u64 = snapshots.iter().map(|s| s.input_tokens).sum();
    let total_output: u64 = snapshots.iter().map(|s| s.output_tokens).sum();

    let mut html = String::with_capacity(8192);
    html.push_str("<!DOCTYPE html><html><head><meta charset='utf-8'>");
    html.push_str("<title>Claw Usage Report</title>");
    html.push_str("<style>");
    html.push_str(concat!(
        "body { font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif; ",
        "max-width: 900px; margin: 0 auto; padding: 20px; background: #1a1a2e; color: #e0e0e0; }"
    ));
    html.push_str("h1, h2 { color: #00d4ff; }");
    html.push_str(
        "table { width: 100%; border-collapse: collapse; margin: 20px 0; }",
    );
    html.push_str(concat!(
        "th, td { padding: 8px 12px; border: 1px solid #333; text-align: left; } ",
        "th { background: #16213e; color: #00d4ff; } ",
        "tr:nth-child(even) { background: #0f3460; }"
    ));
    html.push_str(concat!(
        ".stat { display: inline-block; padding: 15px; margin: 5px; ",
        "background: #16213e; border-radius: 8px; text-align: center; } ",
        ".stat-value { font-size: 24px; font-weight: bold; color: #00d4ff; } ",
        ".stat-label { font-size: 12px; color: #888; }"
    ));
    html.push_str("</style></head><body>");

    // Header
    html.push_str("<h1>Claw Usage Report</h1>");

    // Summary stat cards
    html.push_str("<div>");
    push_stat_card(&mut html, &format!("${total_cost:.2}"), "Total Cost");
    push_stat_card(&mut html, &total_sessions.to_string(), "Sessions");
    push_stat_card(&mut html, &format_tokens(total_input), "Input Tokens");
    push_stat_card(&mut html, &format_tokens(total_output), "Output Tokens");
    html.push_str("</div>");

    // Daily costs table
    html.push_str("<h2>Daily Costs</h2><table><tr><th>Date</th><th>Cost</th></tr>");
    for (date, cost) in &daily {
        let _ = write!(html, "<tr><td>{date}</td><td>${cost:.4}</td></tr>");
    }
    html.push_str("</table>");

    // Model breakdown table
    html.push_str(
        "<h2>By Model</h2><table><tr><th>Model</th><th>Input Tokens</th><th>Output Tokens</th><th>Cost</th></tr>",
    );
    for (model, (inp, out, cost)) in &by_model {
        let _ = write!(
            html,
            "<tr><td>{model}</td><td>{}</td><td>{}</td><td>${cost:.4}</td></tr>",
            format_tokens(*inp),
            format_tokens(*out),
        );
    }
    html.push_str("</table>");

    // Recent sessions table (cap at 50)
    html.push_str(
        "<h2>Recent Sessions</h2><table><tr><th>Session</th><th>Model</th><th>Turns</th><th>Cost</th></tr>",
    );
    for s in snapshots.iter().take(50) {
        let short_id = &s.session_id[..8.min(s.session_id.len())];
        let _ = write!(
            html,
            "<tr><td>{short_id}</td><td>{}</td><td>{}</td><td>${:.4}</td></tr>",
            s.model, s.turn_count, s.cost_usd,
        );
    }
    html.push_str("</table>");

    html.push_str("<p style='color:#555;font-size:12px;'>Generated by Claw Code</p>");
    html.push_str("</body></html>");
    html
}

fn push_stat_card(html: &mut String, value: &str, label: &str) {
    let _ = write!(
        html,
        "<div class='stat'><div class='stat-value'>{value}</div><div class='stat-label'>{label}</div></div>"
    );
}

/// Write the HTML report to the standard location under `config_home/usage-data/report.html`.
///
/// Returns the path to the written file on success.
pub fn write_report(config_home: &Path, html: &str) -> io::Result<PathBuf> {
    let dir = config_home.join("usage-data");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("report.html");
    std::fs::write(&path, html)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(clippy::cast_precision_loss)]
    fn make_snapshot(
        session_id: &str,
        created_at_ms: u64,
        model: &str,
        input_tokens: u64,
        output_tokens: u64,
        turn_count: usize,
    ) -> UsageSnapshot {
        let cost_usd =
            (input_tokens as f64 * 15.0 + output_tokens as f64 * 75.0) / 1_000_000.0;
        UsageSnapshot {
            session_id: session_id.to_string(),
            created_at_ms,
            model: model.to_string(),
            input_tokens,
            output_tokens,
            cost_usd,
            turn_count,
        }
    }

    #[test]
    fn test_aggregate_daily_costs() {
        let snapshots = vec![
            make_snapshot("aaa", 1_736_899_200_000, "sonnet", 1000, 500, 3),
            make_snapshot("bbb", 1_736_899_200_000 + 3_600_000, "sonnet", 2000, 1000, 5),
            // Next day
            make_snapshot("ccc", 1_736_899_200_000 + 86_400_000, "opus", 500, 200, 2),
        ];
        let daily = aggregate_daily_costs(&snapshots);
        assert_eq!(daily.len(), 2);
        // Both first two snapshots should be on the same day
        let first_day = daily.keys().next().unwrap();
        let second_day = daily.keys().nth(1).unwrap();
        assert_ne!(first_day, second_day);
        // All costs should be positive
        for cost in daily.values() {
            assert!(*cost > 0.0);
        }
    }

    #[test]
    fn test_aggregate_by_model() {
        let snapshots = vec![
            make_snapshot("a", 1000, "sonnet", 100, 50, 1),
            make_snapshot("b", 2000, "opus", 200, 100, 2),
            make_snapshot("c", 3000, "sonnet", 300, 150, 3),
        ];
        let by_model = aggregate_by_model(&snapshots);
        assert_eq!(by_model.len(), 2);
        let (sonnet_in, sonnet_out, _) = by_model.get("sonnet").unwrap();
        assert_eq!(*sonnet_in, 400);
        assert_eq!(*sonnet_out, 200);
        let (opus_in, opus_out, _) = by_model.get("opus").unwrap();
        assert_eq!(*opus_in, 200);
        assert_eq!(*opus_out, 100);
    }

    #[test]
    fn test_generate_html_report_contains_sections() {
        let snapshots = vec![
            make_snapshot("session1", 1_700_000_000_000, "sonnet", 5000, 2000, 10),
            make_snapshot("session2", 1_700_100_000_000, "opus", 3000, 1000, 5),
        ];
        let html = generate_html_report(&snapshots);
        assert!(html.contains("<!DOCTYPE html>"));
        assert!(html.contains("Claw Usage Report"));
        assert!(html.contains("Daily Costs"));
        assert!(html.contains("By Model"));
        assert!(html.contains("Recent Sessions"));
        assert!(html.contains("Total Cost"));
        assert!(html.contains("Sessions"));
        assert!(html.contains("sonnet"));
        assert!(html.contains("opus"));
        assert!(html.contains("session1"));
        assert!(html.contains("session2"));
    }

    #[test]
    fn test_format_tokens() {
        assert_eq!(format_tokens(0), "0");
        assert_eq!(format_tokens(500), "500");
        assert_eq!(format_tokens(1_500), "1.5K");
        assert_eq!(format_tokens(999), "999");
        assert_eq!(format_tokens(1_000), "1.0K");
        assert_eq!(format_tokens(1_500_000), "1.5M");
        assert_eq!(format_tokens(2_300_000), "2.3M");
    }

    #[test]
    fn test_empty_snapshots_produces_valid_html() {
        let html = generate_html_report(&[]);
        assert!(html.contains("<!DOCTYPE html>"));
        assert!(html.contains("Claw Usage Report"));
        // Total cost with zero snapshots should be $0.00
        assert!(html.contains("$0.00"));
        assert!(html.contains("</html>"));
    }

    #[test]
    fn test_format_date_from_ms() {
        // 2024-01-01 00:00:00 UTC = 1704067200 seconds
        let ms = 1_704_067_200_000_u64;
        assert_eq!(format_date_from_ms(ms), "2024-01-01");

        // 1970-01-01
        assert_eq!(format_date_from_ms(0), "1970-01-01");

        // 2025-06-15 12:00:00 UTC = 1750075200 seconds
        let ms2 = 1_750_075_200_000_u64;
        assert_eq!(format_date_from_ms(ms2), "2025-06-16");
    }

    #[test]
    fn test_civil_from_days_known_dates() {
        // 1970-01-01 = day 0
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        // 2000-01-01 = day 10957
        assert_eq!(civil_from_days(10957), (2000, 1, 1));
    }

    #[test]
    fn test_write_report() {
        let dir = std::env::temp_dir().join("claw_usage_test");
        let _ = std::fs::remove_dir_all(&dir);
        let result = write_report(&dir, "<html>test</html>");
        assert!(result.is_ok());
        let path = result.unwrap();
        assert!(path.exists());
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, "<html>test</html>");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
