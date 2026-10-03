//! Terminal rendering: colors, tables, and human-friendly formatting.

use std::io::IsTerminal;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use chrono::{Local, TimeZone};
use serde::Serialize;
use unicode_width::UnicodeWidthChar;

static COLOR: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Format {
    /// `table` on a terminal, `json` when piped
    Auto,
    /// Human-readable table
    Table,
    /// Table with extra columns
    Wide,
    /// Normalized JSON (stable schema)
    Json,
    /// Normalized JSON, one object per line for lists
    Jsonl,
    /// The unmodified JSON payload returned by Coroot
    Raw,
}

impl Format {
    pub fn resolve(self) -> Format {
        match self {
            Format::Auto if std::io::stdout().is_terminal() => Format::Table,
            Format::Auto => Format::Json,
            f => f,
        }
    }

    pub fn is_machine(self) -> bool {
        matches!(self.resolve(), Format::Json | Format::Jsonl | Format::Raw)
    }
}

/// The last JSON payload Coroot returned, kept for `--output raw`.
pub type RawSlot = Arc<Mutex<Option<serde_json::Value>>>;

/// Emits command results in the selected format.
#[derive(Clone)]
pub struct Out {
    pub format: Format,
    raw: RawSlot,
}

impl Out {
    pub fn new(format: Format) -> Self {
        Out {
            format: format.resolve(),
            raw: RawSlot::default(),
        }
    }

    pub fn is_machine(&self) -> bool {
        self.format.is_machine()
    }

    pub fn wide(&self) -> bool {
        self.format == Format::Wide
    }

    /// Where the client records payloads; only needed for `--output raw`.
    pub fn raw_slot(&self) -> Option<RawSlot> {
        (self.format == Format::Raw).then(|| self.raw.clone())
    }

    /// Prints `norm` (json/jsonl), the last payload from Coroot (raw), or calls `human`
    /// (table/wide).
    pub fn emit<T: Serialize + ?Sized>(
        &self,
        norm: &T,
        human: impl FnOnce(),
    ) -> anyhow::Result<()> {
        match self.format {
            Format::Json => print_json(norm),
            Format::Jsonl => print_jsonl(&serde_json::to_value(norm)?),
            Format::Raw => match self.raw.lock().ok().and_then(|mut r| r.take()) {
                Some(raw) => print_json(&raw),
                None => print_json(norm),
            },
            _ => {
                human();
                Ok(())
            }
        }
    }

    /// Like `emit`, for results that have no raw form (actions, links).
    pub fn emit_value<T: Serialize + ?Sized>(
        &self,
        norm: &T,
        human: impl FnOnce(),
    ) -> anyhow::Result<()> {
        match self.format {
            Format::Raw => print_json(norm),
            _ => self.emit(norm, human),
        }
    }
}

pub fn init_color(disabled: bool) {
    let enabled =
        !disabled && std::env::var_os("NO_COLOR").is_none() && std::io::stdout().is_terminal();
    COLOR.store(enabled, Ordering::Relaxed);
}

pub fn color_enabled() -> bool {
    COLOR.load(Ordering::Relaxed)
}

fn paint(code: &str, s: &str) -> String {
    if color_enabled() && !s.is_empty() {
        format!("\x1b[{code}m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

pub fn bold(s: &str) -> String {
    paint("1", s)
}
pub fn dim(s: &str) -> String {
    paint("2", s)
}
pub fn red(s: &str) -> String {
    paint("31", s)
}
pub fn green(s: &str) -> String {
    paint("32", s)
}
pub fn yellow(s: &str) -> String {
    paint("33", s)
}
pub fn blue(s: &str) -> String {
    paint("34", s)
}
pub fn cyan(s: &str) -> String {
    paint("36", s)
}

/// Colors a string by a Coroot status name (ok, info, warning, critical, unknown).
pub fn by_status(status: &str, s: &str) -> String {
    match status {
        "ok" => green(s),
        "info" => blue(s),
        "warning" => yellow(s),
        "critical" => red(s),
        _ => dim(s),
    }
}

/// A colored status word, e.g. "critical" in red.
pub fn status(status: &str) -> String {
    by_status(status, status)
}

/// A colored bullet followed by the status word.
pub fn status_dot(status: &str) -> String {
    format!("{} {}", by_status(status, "●"), status)
}

/// Colors a log severity name.
pub fn severity(sev: &str) -> String {
    severity_colored(sev, sev)
}

/// Colors `text` according to the log severity `sev`.
pub fn severity_colored(sev: &str, text: &str) -> String {
    match sev.to_ascii_lowercase().as_str() {
        s if s.starts_with("crit") || s.starts_with("fatal") || s.starts_with("err") => red(text),
        s if s.starts_with("warn") => yellow(text),
        s if s.starts_with("info") => blue(text),
        s if s.starts_with("debug") || s.starts_with("trace") => dim(text),
        _ => text.to_string(),
    }
}

/// Width of a string as displayed in a terminal, ignoring ANSI escape sequences.
pub fn visible_width(s: &str) -> usize {
    let mut width = 0;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
            continue;
        }
        width += c.width().unwrap_or(0);
    }
    width
}

/// Truncates a string to at most `max` visible columns, appending an ellipsis.
/// ANSI sequences are preserved and a reset is appended when the string is cut.
pub fn truncate(s: &str, max: usize) -> String {
    if visible_width(s) <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut width = 0;
    let mut chars = s.chars();
    let mut has_ansi = false;
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            has_ansi = true;
            out.push(c);
            for c in chars.by_ref() {
                out.push(c);
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
            continue;
        }
        let w = c.width().unwrap_or(0);
        if width + w > max - 1 {
            break;
        }
        width += w;
        out.push(c);
    }
    out.push('…');
    if has_ansi {
        out.push_str("\x1b[0m");
    }
    out
}

pub fn terminal_width() -> Option<usize> {
    if !std::io::stdout().is_terminal() {
        return None;
    }
    if let Some(w) = std::env::var("COLUMNS").ok().and_then(|c| c.parse().ok()) {
        return Some(w);
    }
    terminal_size::terminal_size().map(|(w, _)| w.0 as usize)
}

/// A borderless, kubectl-style table. The last column is truncated to fit the terminal.
pub struct Table {
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
}

impl Table {
    pub fn new<S: AsRef<str>>(headers: &[S]) -> Self {
        Table {
            headers: headers.iter().map(|h| h.as_ref().to_uppercase()).collect(),
            rows: Vec::new(),
        }
    }

    pub fn row(&mut self, cells: Vec<String>) {
        self.rows.push(cells);
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn render(&self) -> String {
        let n = self.headers.len();
        let mut widths: Vec<usize> = self.headers.iter().map(|h| visible_width(h)).collect();
        for row in &self.rows {
            for (i, cell) in row.iter().enumerate().take(n) {
                widths[i] = widths[i].max(visible_width(cell));
            }
        }
        // Drop columns that are empty in every row (except the first).
        let keep: Vec<bool> = (0..n)
            .map(|i| {
                i == 0
                    || self
                        .rows
                        .iter()
                        .any(|r| r.get(i).is_some_and(|c| !c.is_empty()))
            })
            .collect();
        let term = terminal_width();
        let mut out = String::new();
        let mut line = |cells: &[String], header: bool| {
            let mut s = String::new();
            let mut used = 0;
            let last = (0..n).rev().find(|&i| keep[i]).unwrap_or(0);
            for i in 0..n {
                if !keep[i] {
                    continue;
                }
                let cell = cells.get(i).map(String::as_str).unwrap_or("");
                let cell = if header { bold(cell) } else { cell.to_string() };
                if i == last {
                    let cell = match term {
                        Some(t) if t > used + 8 => truncate(&cell, t - used),
                        _ => cell,
                    };
                    s.push_str(&cell);
                } else {
                    let pad = widths[i].saturating_sub(visible_width(&cell));
                    s.push_str(&cell);
                    s.push_str(&" ".repeat(pad + 3));
                    used += widths[i] + 3;
                }
            }
            out.push_str(s.trim_end());
            out.push('\n');
        };
        line(&self.headers, true);
        for row in &self.rows {
            line(row, false);
        }
        out
    }

    pub fn print(&self) {
        print!("{}", self.render());
    }
}

/// Prints a section header.
pub fn section(title: &str) {
    println!("\n{}", bold(title));
}

/// Prints an aligned "key: value" listing.
pub fn kv(pairs: &[(&str, String)]) {
    let w = pairs
        .iter()
        .filter(|(_, v)| !v.is_empty())
        .map(|(k, _)| k.len())
        .max()
        .unwrap_or(0);
    for (k, v) in pairs {
        if v.is_empty() {
            continue;
        }
        println!(
            "{}  {}",
            dim(&format!("{:<w$}", format!("{k}:"), w = w + 1)),
            v
        );
    }
}

/// Pretty JSON on a terminal, compact JSON otherwise.
pub fn print_json<T: Serialize + ?Sized>(v: &T) -> anyhow::Result<()> {
    if std::io::stdout().is_terminal() {
        println!("{}", serde_json::to_string_pretty(v)?);
    } else {
        println!("{}", serde_json::to_string(v)?);
    }
    Ok(())
}

/// One JSON document per line; arrays are split into their elements.
pub fn print_jsonl(v: &serde_json::Value) -> anyhow::Result<()> {
    match v {
        serde_json::Value::Array(items) => {
            for item in items {
                println!("{}", serde_json::to_string(item)?);
            }
        }
        other => println!("{}", serde_json::to_string(other)?),
    }
    Ok(())
}

const SPARK: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// Renders values as a unicode sparkline. Missing values are drawn as spaces.
pub fn sparkline(values: &[Option<f64>]) -> String {
    let present: Vec<f64> = values
        .iter()
        .flatten()
        .copied()
        .filter(|v| v.is_finite())
        .collect();
    if present.is_empty() {
        return String::new();
    }
    let min = present.iter().copied().fold(f64::INFINITY, f64::min);
    let max = present.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    values
        .iter()
        .map(|v| match v {
            Some(v) if v.is_finite() => {
                if (max - min).abs() < f64::EPSILON {
                    if max == 0.0 { SPARK[0] } else { SPARK[3] }
                } else {
                    let idx = ((v - min) / (max - min) * 7.0).round() as usize;
                    SPARK[idx.min(7)]
                }
            }
            _ => ' ',
        })
        .collect()
}

/// Downsamples a series to `n` buckets by averaging.
pub fn downsample(values: &[Option<f64>], n: usize) -> Vec<Option<f64>> {
    if values.len() <= n || n == 0 {
        return values.to_vec();
    }
    (0..n)
        .map(|i| {
            let start = i * values.len() / n;
            let end = ((i + 1) * values.len() / n).min(values.len());
            let bucket: Vec<f64> = values[start..end].iter().flatten().copied().collect();
            if bucket.is_empty() {
                None
            } else {
                Some(bucket.iter().sum::<f64>() / bucket.len() as f64)
            }
        })
        .collect()
}

/// Formats a duration given in seconds as a compact string, e.g. "3d4h", "12m", "45s".
pub fn duration(secs: i64) -> String {
    let secs = secs.max(0);
    let (d, h, m, s) = (
        secs / 86400,
        secs % 86400 / 3600,
        secs % 3600 / 60,
        secs % 60,
    );
    match () {
        _ if d > 0 && h > 0 => format!("{d}d{h}h"),
        _ if d > 0 => format!("{d}d"),
        _ if h > 0 && m > 0 => format!("{h}h{m}m"),
        _ if h > 0 => format!("{h}h"),
        _ if m > 0 => format!("{m}m"),
        _ => format!("{s}s"),
    }
}

pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// "5m ago" for a timestamp in epoch milliseconds.
pub fn ago(ms: i64) -> String {
    if ms <= 0 {
        return String::new();
    }
    format!("{} ago", duration((now_ms() - ms) / 1000))
}

/// Local time with milliseconds, used for log lines.
pub fn timestamp_precise(ms: i64) -> String {
    match Local.timestamp_millis_opt(ms) {
        chrono::LocalResult::Single(t) => t.format("%Y-%m-%d %H:%M:%S%.3f").to_string(),
        _ => ms.to_string(),
    }
}

pub fn bytes(v: f64) -> String {
    let units = ["B", "KB", "MB", "GB", "TB", "PB"];
    let mut v = v;
    let mut i = 0;
    while v.abs() >= 1000.0 && i < units.len() - 1 {
        v /= 1000.0;
        i += 1;
    }
    format!("{}{}", number(v), units[i])
}

/// Formats a number with up to 3 significant digits after trimming trailing zeros.
pub fn number(v: f64) -> String {
    if !v.is_finite() {
        return v.to_string();
    }
    let a = v.abs();
    let s = if a == 0.0 {
        "0".to_string()
    } else if a >= 100.0 {
        format!("{v:.0}")
    } else if a >= 10.0 {
        format!("{v:.1}")
    } else if a >= 1.0 {
        format!("{v:.2}")
    } else if a >= 1e-6 {
        // Three significant digits without switching to exponent notation.
        let decimals = (-a.log10().floor()) as usize + 2;
        format!("{v:.decimals$}")
    } else {
        format!("{v:.2e}")
    };
    if s.contains('.') && !s.contains('e') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s
    }
}

/// Formats a duration in seconds with an appropriate unit (µs, ms, s).
pub fn latency(secs: f64) -> String {
    if secs <= 0.0 {
        "0".into()
    } else if secs < 0.001 {
        format!("{}µs", number(secs * 1e6))
    } else if secs < 1.0 {
        format!("{}ms", number(secs * 1e3))
    } else {
        format!("{}s", number(secs))
    }
}

pub fn percent(v: f64) -> String {
    format!("{}%", number(v))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widths_ignore_ansi() {
        assert_eq!(visible_width("\x1b[31mred\x1b[0m"), 3);
        assert_eq!(visible_width("日本"), 4);
    }

    #[test]
    fn truncation() {
        assert_eq!(truncate("hello world", 6), "hello…");
        assert_eq!(truncate("short", 10), "short");
        let t = truncate("\x1b[31mhello world\x1b[0m", 6);
        assert_eq!(visible_width(&t), 6);
        assert!(t.ends_with("\x1b[0m"));
    }

    #[test]
    fn durations() {
        assert_eq!(duration(45), "45s");
        assert_eq!(duration(600), "10m");
        assert_eq!(duration(3600 + 120), "1h2m");
        assert_eq!(duration(86400 * 3 + 3600 * 4), "3d4h");
    }

    #[test]
    fn numbers() {
        assert_eq!(number(0.0), "0");
        assert_eq!(number(1.5), "1.5");
        assert_eq!(number(12.34), "12.3");
        assert_eq!(number(1234.5), "1234");
        assert_eq!(number(0.0123), "0.0123");
        assert_eq!(number(0.000653), "0.000653");
        assert_eq!(number(1.5e-9), "1.50e-9");
        assert_eq!(bytes(1_500_000.0), "1.5MB");
        assert_eq!(latency(0.0123), "12.3ms");
        assert_eq!(latency(2.0), "2s");
    }

    #[test]
    fn sparklines() {
        let s = sparkline(&[Some(0.0), Some(1.0), None, Some(2.0)]);
        assert_eq!(s, "▁▅ █");
        assert_eq!(sparkline(&[None, None]), "");
        assert_eq!(
            downsample(&[Some(1.0), Some(3.0), Some(5.0), Some(7.0)], 2),
            vec![Some(2.0), Some(6.0)]
        );
    }

    #[test]
    fn table_drops_empty_columns() {
        let mut t = Table::new(&["name", "empty", "value"]);
        t.row(vec!["a".into(), "".into(), "1".into()]);
        let out = t.render();
        assert!(!out.contains("EMPTY"));
        assert!(out.contains("NAME   VALUE"));
    }
}
