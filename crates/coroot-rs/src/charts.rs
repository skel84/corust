//! Chart histories from an application's report widgets.
//!
//! `GET app/<id>` carries the full chart widgets of every report: series arrays, a time
//! context and annotations. This module decodes them strictly. See [`Project::app_charts`]
//! for what Coroot does and does not send.

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::client::Envelope;
use crate::error::{Error, Result};
use crate::id::AppId;
use crate::json::{self, null_default, rfc3339_millis, seconds};
use crate::project::Project;
use crate::status::Status;
use crate::util::encode_segment;

/// Bounds on what [`Project::app_charts_with`] retains. A response over a bound fails
/// with [`ErrorKind::Decode`](crate::ErrorKind::Decode); nothing is cut off silently.
/// Every bound must be at least 1, otherwise the call fails with
/// [`ErrorKind::InvalidInput`](crate::ErrorKind::InvalidInput) before any request is sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChartLimits {
    /// Reports in the response.
    pub max_reports: usize,
    /// Charts in all reports together.
    pub max_charts: usize,
    /// Series per chart (the threshold series is not counted).
    pub max_series: usize,
    /// Samples per series.
    pub max_points: usize,
    /// Annotations per chart.
    pub max_annotations: usize,
    /// Samples in the whole response.
    pub max_total_samples: usize,
}

impl Default for ChartLimits {
    fn default() -> Self {
        ChartLimits {
            max_reports: 64,
            max_charts: 256,
            max_series: 64,
            max_points: 10_000,
            max_annotations: 256,
            max_total_samples: 1_000_000,
        }
    }
}

impl ChartLimits {
    fn validate(&self) -> Result<()> {
        for (name, v) in [
            ("max_reports", self.max_reports),
            ("max_charts", self.max_charts),
            ("max_series", self.max_series),
            ("max_points", self.max_points),
            ("max_annotations", self.max_annotations),
            ("max_total_samples", self.max_total_samples),
        ] {
            if v == 0 {
                return Err(Error::invalid(format!(
                    "ChartLimits::{name} must be at least 1"
                )));
            }
        }
        Ok(())
    }
}

/// An application's report charts over the project's time window.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppCharts {
    /// The application, as confirmed by the response.
    #[serde(with = "crate::id::as_string")]
    pub app: AppId,
    /// Every report in the response, in Coroot's order, including those without charts.
    pub reports: Vec<ReportCharts>,
}

impl AppCharts {
    /// All charts with the name of their report.
    pub fn charts(&self) -> impl Iterator<Item = (&str, &ChartHistory)> {
        self.reports
            .iter()
            .flat_map(|r| r.charts.iter().map(move |c| (r.name.as_str(), c)))
    }
}

/// The charts of one report (`SLO`, `CPU`, `Memory`, ...).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReportCharts {
    /// The report name.
    pub name: String,
    /// The report's status.
    pub status: Status,
    /// Its charts, with those of chart groups flattened in order.
    #[serde(default, deserialize_with = "null_default")]
    pub charts: Vec<ChartHistory>,
}

/// One chart: its time context, series and annotations.
///
/// Coroot sends no units and no per-point timestamps. Sample *i* of every series is at
/// [`ChartHistory::point_time`]`(i)`, which is how Coroot's UI places it. The title and
/// series names are the only description of what is measured.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChartHistory {
    /// The title of the chart group this chart belongs to, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// The chart title. Coroot appends `(truncated range)` to it when it shortened the
    /// window (to the group title for grouped charts).
    pub title: String,
    /// The chart's raw window start, as Coroot sent it. Sample 0 is at
    /// [`ChartHistory::anchor`], which is this value truncated to a multiple of `step`.
    #[serde(with = "rfc3339_millis")]
    pub from: DateTime<Utc>,
    /// End of the chart's window.
    #[serde(with = "rfc3339_millis")]
    pub to: DateTime<Utc>,
    /// Spacing between samples.
    #[serde(rename = "step_seconds", with = "seconds")]
    pub step: Duration,
    /// Whether Coroot shortened the requested window. For a chart in a group Coroot clears
    /// the flag and puts `(truncated range)` on the group title instead; this is `true` in
    /// that case too.
    #[serde(default)]
    pub truncated: bool,
    /// Whether the series are meant to be stacked.
    #[serde(default)]
    pub stacked: bool,
    /// The series.
    #[serde(default, deserialize_with = "null_default")]
    pub series: Vec<SeriesHistory>,
    /// The threshold line, if the chart has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threshold: Option<SeriesHistory>,
    /// Marked time spans, e.g. incidents.
    #[serde(default, deserialize_with = "null_default")]
    pub annotations: Vec<ChartAnnotation>,
}

impl ChartHistory {
    /// The time of sample 0: `from` truncated down to a multiple of `step`.
    ///
    /// Coroot reads the data from the truncated bounds (`from.Truncate(step)`), so this is
    /// where sample 0 really is. Coroot's UI places it at the raw `ctx.from` instead,
    /// which is up to one step earlier than the data; this crate uses the data's anchor.
    pub fn anchor(&self) -> DateTime<Utc> {
        let step = self.step.as_millis() as i64;
        let ms = self.from.timestamp_millis();
        json::time_ms(ms - ms.rem_euclid(step)).unwrap_or(self.from)
    }

    /// The time of sample `i`: [`ChartHistory::anchor`]` + i * step`.
    pub fn point_time(&self, i: usize) -> Option<DateTime<Utc>> {
        let offset = self.step.checked_mul(u32::try_from(i).ok()?)?;
        self.anchor()
            .checked_add_signed(chrono::Duration::from_std(offset).ok()?)
    }

    /// How many samples a complete series has: `(trunc(to) - trunc(from)) / step + 1`,
    /// both bounds truncated to a multiple of `step` as Coroot does when it reads the data.
    pub fn expected_points(&self) -> usize {
        let step = self.step.as_millis() as i64;
        let trunc = |t: DateTime<Utc>| {
            let ms = t.timestamp_millis();
            ms - ms.rem_euclid(step)
        };
        ((trunc(self.to) - trunc(self.from)).max(0) / step) as usize + 1
    }
}

/// How much of a chart's window a series covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SeriesCoverage {
    /// The series has all the samples its chart's window implies.
    Full,
    /// The series is shorter than the window. Coroot does not send where it starts, so
    /// the samples are placed from the chart start and the rest is unknown.
    Partial,
    /// Coroot sent no data for the series (`data: null`).
    Empty,
}

/// One series of a chart.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SeriesHistory {
    /// The series name.
    pub name: String,
    /// A display title, when it differs from the name.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    /// One value per step. `None` marks a missing sample; Coroot sends NaN and infinite
    /// values both as `null`, so those two cannot be told apart.
    #[serde(default, deserialize_with = "null_default")]
    pub samples: Vec<Option<f64>>,
    /// How much of the window the samples cover.
    pub coverage: SeriesCoverage,
}

impl SeriesHistory {
    /// The number of samples that are present (not missing).
    pub fn present(&self) -> usize {
        self.samples.iter().flatten().count()
    }
}

/// A marked time span on a chart.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChartAnnotation {
    /// What is marked, e.g. `incident`.
    pub name: String,
    /// Start of the span; `None` when Coroot sent none.
    #[serde(
        default,
        with = "json::rfc3339::option",
        skip_serializing_if = "Option::is_none"
    )]
    pub from: Option<DateTime<Utc>>,
    /// End of the span; `None` when Coroot sent none.
    #[serde(
        default,
        with = "json::rfc3339::option",
        skip_serializing_if = "Option::is_none"
    )]
    pub to: Option<DateTime<Utc>>,
}

const TRUNCATED_SUFFIX: &str = "(truncated range)";

fn shape(field: &str) -> Error {
    Error::decode(format!(
        "unexpected application response: invalid or missing {field}"
    ))
}

fn over(limit: &str) -> Error {
    Error::decode(format!("application response exceeds ChartLimits::{limit}"))
}

/// A list that may be absent or `null` (Go's empty slice) but never anything but objects.
fn list<'a>(v: &'a Value, field: &str) -> Result<&'a [Value]> {
    match v.get(field) {
        None | Some(Value::Null) => Ok(&[]),
        Some(Value::Array(a)) if a.iter().all(Value::is_object) => Ok(a),
        _ => Err(shape(field)),
    }
}

fn required_str<'a>(v: &'a Value, field: &str) -> Result<&'a str> {
    v.get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| shape(field))
}

fn optional_str<'a>(v: &'a Value, field: &str) -> Result<&'a str> {
    match v.get(field) {
        None | Some(Value::Null) => Ok(""),
        Some(Value::String(s)) => Ok(s),
        _ => Err(shape(field)),
    }
}

fn flag(v: &Value, field: &str) -> Result<bool> {
    match v.get(field) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(b)) => Ok(*b),
        _ => Err(shape(field)),
    }
}

/// An epoch-milliseconds field that is `null` or absent when the server's value is zero.
fn optional_time(v: &Value, field: &str) -> Result<Option<DateTime<Utc>>> {
    match v.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(n) => match n.as_i64() {
            Some(ms) if ms > 0 => json::time_ms(ms).map(Some).ok_or_else(|| shape(field)),
            _ => Err(shape(field)),
        },
    }
}

struct Budget<'a> {
    limits: &'a ChartLimits,
    charts: usize,
    samples: usize,
}

fn context(ctx: &Value) -> Result<(DateTime<Utc>, DateTime<Utc>, Duration, bool)> {
    if !ctx.is_object() {
        return Err(shape("chart ctx"));
    }
    let from = optional_time(ctx, "from")?.ok_or_else(|| shape("chart ctx.from"))?;
    let to = optional_time(ctx, "to")?.ok_or_else(|| shape("chart ctx.to"))?;
    let step_ms = ctx
        .get("step")
        .and_then(Value::as_u64)
        .filter(|s| *s > 0 && s % 1000 == 0)
        .ok_or_else(|| shape("chart ctx.step"))?;
    if to <= from {
        return Err(shape("chart ctx (to is not after from)"));
    }
    Ok((
        from,
        to,
        Duration::from_millis(step_ms),
        flag(ctx, "truncated")?,
    ))
}

fn series(v: &Value, expected: usize, budget: &mut Budget<'_>) -> Result<SeriesHistory> {
    if !v.is_object() {
        return Err(shape("series"));
    }
    let samples: Vec<Option<f64>> = match v.get("data") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(a)) => {
            if a.len() > budget.limits.max_points {
                return Err(over("max_points"));
            }
            // Coroot never sends more than (trunc(to) - trunc(from)) / step + 1 points.
            if a.len() > expected {
                return Err(shape("series data (longer than the chart window)"));
            }
            a.iter()
                .map(|x| match x {
                    Value::Null => Ok(None),
                    Value::Number(n) => n.as_f64().map(Some).ok_or_else(|| shape("series data")),
                    _ => Err(shape("series data")),
                })
                .collect::<Result<_>>()?
        }
        _ => return Err(shape("series data")),
    };
    budget.samples += samples.len();
    if budget.samples > budget.limits.max_total_samples {
        return Err(over("max_total_samples"));
    }
    let coverage = if samples.is_empty() {
        SeriesCoverage::Empty
    } else if samples.len() < expected {
        SeriesCoverage::Partial
    } else {
        SeriesCoverage::Full
    };
    Ok(SeriesHistory {
        name: optional_str(v, "name")?.to_string(),
        title: optional_str(v, "title")?.to_string(),
        samples,
        coverage,
    })
}

fn chart(v: &Value, group: Option<&str>, budget: &mut Budget<'_>) -> Result<ChartHistory> {
    budget.charts += 1;
    if budget.charts > budget.limits.max_charts {
        return Err(over("max_charts"));
    }
    let (from, to, step, mut truncated) = context(v.get("ctx").unwrap_or(&Value::Null))?;
    // Chart groups clear the flag and mark their own title instead.
    truncated |= group.is_some_and(|g| g.ends_with(TRUNCATED_SUFFIX));
    let mut out = ChartHistory {
        group: group.map(str::to_string),
        title: required_str(v, "title")?.to_string(),
        from,
        to,
        step,
        truncated,
        stacked: flag(v, "stacked")?,
        series: Vec::new(),
        threshold: None,
        annotations: Vec::new(),
    };
    let expected = out.expected_points();
    let all = list(v, "series")?;
    if all.len() > budget.limits.max_series {
        return Err(over("max_series"));
    }
    for s in all {
        out.series.push(series(s, expected, budget)?);
    }
    if let Some(t) = v.get("threshold").filter(|t| !t.is_null()) {
        out.threshold = Some(series(t, expected, budget)?);
    }
    let anns = list(v, "annotations")?;
    if anns.len() > budget.limits.max_annotations {
        return Err(over("max_annotations"));
    }
    for a in anns {
        out.annotations.push(ChartAnnotation {
            name: optional_str(a, "name")?.to_string(),
            from: optional_time(a, "x1")?,
            to: optional_time(a, "x2")?,
        });
    }
    Ok(out)
}

/// Checks the `{context, data}` envelope of `GET app/<id>` and returns `data.reports`.
/// `data: null` (Coroot has no world for the project yet) is an error, never "no reports".
pub(crate) fn app_reports<'a>(env: &'a Envelope, app: &AppId) -> Result<&'a [Value]> {
    if !env.context.is_object() {
        return Err(shape("{context, data} envelope"));
    }
    if !env.data.is_object() {
        return Err(shape(
            "application data (does Coroot have data for the project yet?)",
        ));
    }
    if json::sp(&env.data, "/app_map/application/id") != app.as_str() {
        return Err(shape("application identity does not match request"));
    }
    match env.data.get("reports") {
        Some(Value::Array(a)) if a.iter().all(Value::is_object) => Ok(a),
        _ => Err(shape("reports")),
    }
}

fn app_charts_from(env: &Envelope, app: &AppId, limits: &ChartLimits) -> Result<AppCharts> {
    let reports = app_reports(env, app)?;
    if reports.len() > limits.max_reports {
        return Err(over("max_reports"));
    }
    let mut budget = Budget {
        limits,
        charts: 0,
        samples: 0,
    };
    let mut out = Vec::with_capacity(reports.len());
    for r in reports {
        let mut charts = Vec::new();
        for w in list(r, "widgets")? {
            if let Some(c) = w.get("chart").filter(|c| !c.is_null()) {
                charts.push(chart(c, None, &mut budget)?);
            }
            if let Some(g) = w.get("chart_group").filter(|g| !g.is_null()) {
                let title = optional_str(g, "title")?;
                for c in list(g, "charts")? {
                    charts.push(chart(c, Some(title), &mut budget)?);
                }
            }
        }
        out.push(ReportCharts {
            name: required_str(r, "name")?.to_string(),
            status: Status::parse(required_str(r, "status")?),
            charts,
        });
    }
    Ok(AppCharts {
        app: app.clone(),
        reports: out,
    })
}

impl Project {
    /// The chart histories of an application's reports, with [`ChartLimits::default`].
    /// See [`Project::app_charts_with`].
    pub async fn app_charts(&self, app: &AppId) -> Result<AppCharts> {
        self.app_charts_with(app, ChartLimits::default()).await
    }

    /// The chart histories of an application's reports: `GET app/<id>` over the project's
    /// time window, decoded strictly. Works with any credentials.
    ///
    /// What Coroot sends, and so what you get:
    ///
    /// - series values and a time context (`from`, `to`, `step`); sample *i* is at
    ///   [`ChartHistory::point_time`]: `from` truncated to the step, plus `i * step` (Coroot's
    ///   UI draws it from the raw `from`, up to one step earlier). There are **no per-point timestamps and no
    ///   units**; use the chart title and series names, and treat units as unknown.
    /// - NaN and infinite samples both arrive as `null` and become `None`.
    /// - a series shorter than its window is reported as [`SeriesCoverage::Partial`]; an
    ///   absent one as [`SeriesCoverage::Empty`]. Neither is padded.
    /// - `data: null`, an application other than the one asked for, a malformed context,
    ///   or a sample that is not a number or `null` fails with
    ///   [`ErrorKind::Decode`](crate::ErrorKind::Decode). A response over `limits`
    ///   fails the same way; invalid `limits` fail with
    ///   [`ErrorKind::InvalidInput`](crate::ErrorKind::InvalidInput) before any I/O.
    ///
    /// The window the server uses is the project's range (the last hour by default), and
    /// its step is chosen by the server. History beyond that window is not available
    /// through this call. Nothing is derived from the summary values of
    /// [`Project::app_health`].
    pub async fn app_charts_with(&self, app: &AppId, limits: ChartLimits) -> Result<AppCharts> {
        limits.validate()?;
        if app.as_str().is_empty() {
            return Err(Error::invalid("application id is empty"));
        }
        let env = self
            .get(&format!("app/{}", encode_segment(app.as_str())), &[])
            .await?;
        app_charts_from(&env, app, &limits)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const APP: &str = "c1:shop:Deployment:api";

    fn ctx() -> Value {
        // Aligned bounds: (190000 - 10000) / 30000 + 1 = 7 points, 30 s apart.
        json!({"from": 1790000010000_i64, "to": 1790000190000_i64, "step": 30000,
            "raw_step": 15000, "truncated": false})
    }

    fn chart_json(data: Value) -> Value {
        json!({"ctx": ctx(), "title": "Latency, seconds", "stacked": false,
            "series": [{"name": "p95", "data": data}], "threshold": null,
            "annotations": [{"name": "incident", "x1": 1790000070000_i64, "x2": null, "icon": ""}]})
    }

    fn env(reports: Value) -> Envelope {
        Envelope {
            context: json!({}),
            data: json!({"app_map": {"application": {"id": APP}}, "reports": reports}),
        }
    }

    fn parse(reports: Value) -> Result<AppCharts> {
        app_charts_from(&env(reports), &AppId::new(APP), &ChartLimits::default())
    }

    fn report(widgets: Value) -> Value {
        json!([{"name": "SLO", "status": "ok", "widgets": widgets, "checks": []}])
    }

    #[test]
    fn places_samples_and_keeps_gaps() {
        let c = parse(report(
            json!([{"chart": chart_json(json!([0.1, null, 0.3, 0.0, null, 0.5, 0.6]))}]),
        ))
        .unwrap();
        let (name, ch) = c.charts().next().unwrap();
        assert_eq!(name, "SLO");
        let s = &ch.series[0];
        assert_eq!(
            s.samples,
            [
                Some(0.1),
                None,
                Some(0.3),
                Some(0.0),
                None,
                Some(0.5),
                Some(0.6)
            ]
        );
        assert_eq!(s.present(), 5);
        assert_eq!(s.coverage, SeriesCoverage::Full);
        assert_eq!(ch.step, Duration::from_secs(30));
        assert_eq!(ch.expected_points(), 7);
        assert_eq!(ch.point_time(2).unwrap().timestamp_millis(), 1790000070000);
        assert_eq!(
            ch.annotations[0].from.unwrap().timestamp_millis(),
            1790000070000
        );
        assert!(ch.annotations[0].to.is_none());
        // Round trip through the stable JSON form.
        let back: AppCharts = serde_json::from_value(serde_json::to_value(&c).unwrap()).unwrap();
        assert_eq!(back, c);
    }

    #[test]
    fn unaligned_bounds_give_one_more_point_anchored_at_the_truncated_start() {
        // Raw from is 15 s past a step boundary; the data starts at the boundary and has
        // (trunc(to) - trunc(from)) / step + 1 = 7 points, one more than raw span / step (5).
        let mut ch = chart_json(json!([1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0]));
        ch["ctx"]["from"] = json!(1790000025000_i64);
        ch["ctx"]["to"] = json!(1790000199000_i64);
        let c = parse(report(json!([{"chart": ch.clone()}]))).unwrap();
        let h = &c.reports[0].charts[0];
        assert_eq!(h.expected_points(), 7);
        assert_eq!(h.series[0].coverage, SeriesCoverage::Full);
        assert_eq!(h.anchor().timestamp_millis(), 1790000010000);
        assert_eq!(h.point_time(0).unwrap().timestamp_millis(), 1790000010000);
        assert_eq!(h.point_time(6).unwrap().timestamp_millis(), 1790000190000);
        // One point too many is still malformed.
        ch["series"][0]["data"] = json!([1, 2, 3, 4, 5, 6, 7, 8]);
        assert!(parse(report(json!([{"chart": ch}]))).is_err());
    }

    #[test]
    fn grouped_charts_read_truncation_from_the_group_title() {
        let g = |title: &str| {
            parse(report(
                json!([{"chart_group": {"title": title, "charts": [chart_json(json!([1.0]))]}}]),
            ))
            .unwrap()
        };
        assert!(g("Containers (truncated range)").reports[0].charts[0].truncated);
        assert!(!g("Containers").reports[0].charts[0].truncated);
        // A plain chart keeps the flag from its own ctx.
        let mut ch = chart_json(json!([1.0]));
        ch["ctx"]["truncated"] = json!(true);
        assert!(parse(report(json!([{"chart": ch}]))).unwrap().reports[0].charts[0].truncated);
    }

    #[test]
    fn short_and_empty_series_are_not_padded() {
        let c = parse(report(json!([{"chart": chart_json(json!([1.0, 2.0]))},
            {"chart": chart_json(Value::Null)}])))
        .unwrap();
        let cov: Vec<_> = c.charts().map(|(_, ch)| ch.series[0].coverage).collect();
        assert_eq!(cov, [SeriesCoverage::Partial, SeriesCoverage::Empty]);
        assert_eq!(c.reports[0].charts[0].series[0].samples.len(), 2);
    }

    #[test]
    fn flattens_chart_groups_and_keeps_report_without_charts() {
        let reports = json!([
            {"name": "CPU", "status": "warning", "widgets": [
                {"chart_group": {"title": "Containers", "charts": [chart_json(json!([1.0]))]}},
                {"table": {"header": [], "rows": []}}]},
            {"name": "Logs", "status": "ok", "widgets": null}]);
        let c = parse(reports).unwrap();
        assert_eq!(c.reports.len(), 2);
        assert_eq!(c.reports[0].charts[0].group.as_deref(), Some("Containers"));
        assert!(c.reports[1].charts.is_empty());
        assert_eq!(c.reports[0].status, Status::Warning);
    }

    #[test]
    fn rejects_malformed_responses() {
        let ok = chart_json(json!([1.0]));
        let with = |f: &dyn Fn(&mut Value)| {
            let mut c = ok.clone();
            f(&mut c);
            report(json!([{"chart": c}]))
        };
        let bad = [
            with(&|c| c["ctx"]["step"] = json!(0)),
            with(&|c| c["ctx"]["step"] = json!(1500)),
            with(&|c| c["ctx"]["to"] = c["ctx"]["from"].clone()),
            with(&|c| c["ctx"] = Value::Null),
            with(&|c| c["title"] = json!(1)),
            with(&|c| c["series"] = json!({})),
            with(&|c| c["series"][0]["data"] = json!(["1"])),
            with(&|c| c["series"][0]["data"] = json!(true)),
            with(&|c| c["series"][0]["data"] = json!([0, 0, 0, 0, 0, 0, 0, 0])),
            with(&|c| c["annotations"][0]["x1"] = json!("soon")),
            json!([{"status": "ok", "widgets": []}]),
            json!([null]),
            json!({}),
            Value::Null,
        ];
        for reports in bad {
            let e = parse(reports.clone()).unwrap_err();
            assert_eq!(e.kind(), crate::ErrorKind::Decode, "{reports}");
        }
    }

    #[test]
    fn rejects_bad_envelopes_and_wrong_application() {
        let app = AppId::new(APP);
        let limits = ChartLimits::default();
        let envs = [
            Envelope {
                context: Value::Null,
                data: json!({"reports": []}),
            },
            Envelope {
                context: json!({}),
                data: Value::Null,
            },
            Envelope {
                context: json!({}),
                data: json!([]),
            },
            Envelope {
                context: json!({}),
                data: json!({"app_map": {"application": {"id": "c1:shop:Deployment:other"}}, "reports": []}),
            },
        ];
        for e in envs {
            assert_eq!(
                app_charts_from(&e, &app, &limits).unwrap_err().kind(),
                crate::ErrorKind::Decode
            );
        }
        // Empty report list for the right application is a real, empty answer.
        assert!(parse(json!([])).unwrap().reports.is_empty());
    }

    #[test]
    fn enforces_limits() {
        let response = env(report(
            json!([{"chart": chart_json(json!([1.0, 2.0, 3.0]))}]),
        ));
        let app = AppId::new(APP);
        let base = ChartLimits::default();
        let zero = ChartLimits {
            max_reports: 0,
            ..base
        };
        assert_eq!(
            zero.validate().unwrap_err().kind(),
            crate::ErrorKind::InvalidInput
        );
        for limits in [
            ChartLimits {
                max_charts: 1,
                max_points: 2,
                ..base
            },
            ChartLimits {
                max_total_samples: 2,
                ..base
            },
            ChartLimits {
                max_series: 1,
                max_annotations: 1,
                max_points: 2,
                ..base
            },
        ] {
            assert_eq!(
                app_charts_from(&response, &app, &limits)
                    .unwrap_err()
                    .kind(),
                crate::ErrorKind::Decode
            );
        }
        let mut two = chart_json(json!([1.0]));
        two["series"] = json!([{"name": "a", "data": [1.0]}, {"name": "b", "data": [1.0]}]);
        let e = env(report(json!([{"chart": two}])));
        let tight = ChartLimits {
            max_series: 1,
            ..base
        };
        assert!(app_charts_from(&e, &app, &tight).is_err());
        let ann = ChartLimits {
            max_annotations: 1,
            ..base
        };
        assert!(app_charts_from(&response, &app, &ann).is_ok());
    }
}
