//! An application's chart histories in a window around one deployment revision.
//!
//! See [`Project::charts_around`] for what the window holds and what it cannot say.

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::charts::{AppCharts, ChartHistory, ChartLimits, SeriesCoverage, SeriesHistory};
use crate::error::{Error, ErrorKind, Result};
use crate::id::AppId;
use crate::project::Project;
use crate::revisions::DeploymentRevision;
use crate::time::TimeRange;

/// How much time [`Project::charts_around`] reads on each side of a revision's start.
///
/// The default is 30 minutes each way, the window Coroot's own deployment link opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RevisionWindow {
    /// Time before the revision started.
    pub before: Duration,
    /// Time after the revision started.
    pub after: Duration,
}

impl RevisionWindow {
    /// The shortest side: one minute. A shorter side holds a sample or two at most of
    /// Coroot's steps.
    pub const MIN_SIDE: Duration = Duration::from_secs(60);
    /// The longest side: twelve hours.
    pub const MAX_SIDE: Duration = Duration::from_secs(12 * 3600);

    /// A window of `side` on both sides of the start.
    pub fn both(side: Duration) -> Self {
        RevisionWindow {
            before: side,
            after: side,
        }
    }

    /// The window around `started_at` as a [`TimeRange`], for a caller that reads
    /// `GET app/<id>` itself with [`Project::get`] to decode more of the answer than
    /// [`Project::charts_around`] does. It is checked and rounded as `charts_around` checks
    /// and rounds it: an invalid side fails with [`ErrorKind::InvalidInput`].
    pub fn range(&self, started_at: DateTime<Utc>) -> Result<TimeRange> {
        self.validate()?;
        let (from, to) = self.bounds(started_at)?;
        Ok(TimeRange::between(from, to))
    }

    fn validate(&self) -> Result<()> {
        for (name, side) in [("before", self.before), ("after", self.after)] {
            if side < Self::MIN_SIDE || side > Self::MAX_SIDE {
                return Err(Error::invalid(format!(
                    "RevisionWindow::{name} must be between 1 minute and 12 hours"
                )));
            }
        }
        Ok(())
    }

    /// The window's bounds around `started_at`, in whole seconds: Coroot reads `from` and
    /// `to` as milliseconds but keeps only the seconds, and its charts carry those back.
    fn bounds(&self, started_at: DateTime<Utc>) -> Result<(DateTime<Utc>, DateTime<Utc>)> {
        let side = |d: Duration| chrono::Duration::from_std(d).ok();
        let whole = |t: DateTime<Utc>| DateTime::from_timestamp(t.timestamp(), 0);
        let from = side(self.before)
            .and_then(|d| started_at.checked_sub_signed(d))
            .and_then(whole);
        let to = side(self.after)
            .and_then(|d| started_at.checked_add_signed(d))
            .and_then(whole);
        match (from, to) {
            (Some(f), Some(t)) => Ok((f, t)),
            _ => Err(Error::invalid("revision window is out of range")),
        }
    }
}

impl Default for RevisionWindow {
    fn default() -> Self {
        RevisionWindow::both(Duration::from_secs(30 * 60))
    }
}

/// What Coroot answered for the window around a revision.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "outcome")]
#[non_exhaustive]
pub enum AroundRevision {
    /// Coroot answered with the application's charts for the window.
    Charts(RevisionCharts),
    /// Coroot answered 404 "Application not found" for this window
    /// ([`AroundRevision::is_no_data`]): the application has no metrics in the window
    /// Coroot built. That is the case when the window starts after Coroot's newest data,
    /// when the application sent no metrics in it (it wasn't running, or the window is
    /// older than the cache keeps, 30 days by default), and also when the application id
    /// names no application at all. The answer can't tell these apart, so a revision must
    /// come from the same application ([`Project::deployment_revisions`]); then this
    /// means no data for the window, not a missing application.
    NoData {
        /// The revision id.
        revision: String,
        /// Start of the window asked for.
        #[serde(with = "crate::json::rfc3339_millis")]
        from: DateTime<Utc>,
        /// End of the window asked for.
        #[serde(with = "crate::json::rfc3339_millis")]
        to: DateTime<Utc>,
    },
}

/// The charts of an application around one revision's start.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RevisionCharts {
    /// The revision id, `<hash>:<start unix seconds>`.
    pub revision: String,
    /// When the rollout started (second precision, from the id).
    #[serde(with = "crate::json::rfc3339_millis")]
    pub started_at: DateTime<Utc>,
    /// Start of the window asked for.
    #[serde(with = "crate::json::rfc3339_millis")]
    pub from: DateTime<Utc>,
    /// End of the window asked for. A chart whose own `to` is earlier ends early; see
    /// [`RevisionCharts::ends_early`].
    #[serde(with = "crate::json::rfc3339_millis")]
    pub to: DateTime<Utc>,
    /// The charts, decoded as [`Project::app_charts`] decodes them.
    pub charts: AppCharts,
}

impl AroundRevision {
    /// Whether `error` is Coroot's own 404 for an application missing from the window it
    /// built, the body `Application not found`. Any other 404, such as a proxy's page,
    /// isn't: it says nothing about the window. For a caller that reads the window with
    /// [`Project::get`] over [`RevisionWindow::range`].
    pub fn is_no_data(error: &Error) -> bool {
        error.kind() == ErrorKind::NotFound && error.message().trim() == NO_APPLICATION
    }
}

/// The body of Coroot's 404 for an application missing from the window it built.
const NO_APPLICATION: &str = "Application not found";

impl RevisionCharts {
    /// Where each chart's samples fall around the revision's start.
    pub fn split(&self, chart: &ChartHistory) -> ChartSplit {
        chart.split_at(self.started_at)
    }

    /// Whether `chart` stops before the end of the window asked for. Coroot ends a window
    /// at the newest data it has, so this is the case for a revision that started less than
    /// [`RevisionWindow::after`] ago, or when Coroot's cache is behind.
    pub fn ends_early(&self, chart: &ChartHistory) -> bool {
        chart.to < self.to
    }
}

/// Where a chart's samples fall around an instant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChartSplit {
    /// The number of sample places before the instant. Sample `before` is the first at or
    /// after it.
    pub before: usize,
    /// The number of sample places at or after the instant.
    pub after: usize,
}

/// How one series covers one side of an instant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "coverage")]
#[non_exhaustive]
pub enum SideCoverage {
    /// Every sample on this side is present.
    Full,
    /// Some samples on this side are missing.
    Gaps {
        /// Samples present on this side.
        present: usize,
        /// Sample places on this side.
        expected: usize,
    },
    /// No sample on this side is present.
    Empty,
    /// The series is shorter than its window ([`SeriesCoverage::Partial`]) and Coroot does
    /// not send where it starts, so which side its samples fall on is unknown. A split
    /// made for another chart, whose places run past this series' samples, is unplaced
    /// too.
    Unplaced,
}

impl ChartHistory {
    /// Where this chart's sample places fall around `at`: those whose time
    /// ([`ChartHistory::point_time`]) is before it, and the rest.
    pub fn split_at(&self, at: DateTime<Utc>) -> ChartSplit {
        let n = self.expected_points();
        let step = (self.step.as_millis() as i64).max(1);
        let offset = at.timestamp_millis() - self.anchor().timestamp_millis();
        // The first place at or after `at`: ceil(offset / step), kept within the window.
        let first = if offset <= 0 {
            0
        } else {
            usize::try_from((offset + step - 1) / step).unwrap_or(usize::MAX)
        };
        let before = first.min(n);
        ChartSplit {
            before,
            after: n - before,
        }
    }
}

impl SeriesHistory {
    /// How this series covers each side of `split`, before and after. `None` for a side
    /// the window has no sample place on.
    pub fn sides(&self, split: &ChartSplit) -> (Option<SideCoverage>, Option<SideCoverage>) {
        let side = |range: std::ops::Range<usize>| {
            if range.is_empty() {
                return None;
            }
            Some(match self.coverage {
                SeriesCoverage::Partial => SideCoverage::Unplaced,
                SeriesCoverage::Empty => SideCoverage::Empty,
                SeriesCoverage::Full => {
                    let expected = range.len();
                    // A split from another chart can run past the samples.
                    let Some(samples) = self.samples.get(range) else {
                        return Some(SideCoverage::Unplaced);
                    };
                    let present = samples.iter().flatten().count();
                    match present {
                        0 => SideCoverage::Empty,
                        p if p == expected => SideCoverage::Full,
                        p => SideCoverage::Gaps {
                            present: p,
                            expected,
                        },
                    }
                }
            })
        };
        let at = split.before;
        (side(0..at), side(at..at.saturating_add(split.after)))
    }
}

impl Project {
    /// The application's charts around `revision`'s start, with [`ChartLimits::default`].
    /// See [`Project::charts_around_with`].
    pub async fn charts_around(
        &self,
        app: &AppId,
        revision: &DeploymentRevision,
        window: RevisionWindow,
    ) -> Result<AroundRevision> {
        self.charts_around_with(app, revision, window, ChartLimits::default())
            .await
    }

    /// The application's charts from `window.before` before `revision` started to
    /// `window.after` after it: one `GET app/<id>` over that window, whatever this
    /// project's own range is. Works with any credentials.
    ///
    /// The charts are decoded as [`Project::app_charts_with`] decodes them, and the same
    /// limits hold: no units, no per-point timestamps, NaN and infinite samples both
    /// `None`. On top of that:
    ///
    /// - [`ChartHistory::split_at`] (or [`RevisionCharts::split`]) says which sample places
    ///   are before the start and which after, and [`SeriesHistory::sides`] how each side is
    ///   covered. A series shorter than its window has no known place, so both its sides are
    ///   [`SideCoverage::Unplaced`].
    /// - **The start is all that is known.** Coroot's REST API does not send when the
    ///   rollout finished, so "after" includes the rollout itself. Its own findings
    ///   ([`DeploymentRevision::findings`]) are measured on a snapshot taken after the
    ///   rollout finished; nothing here recomputes them.
    /// - **The first sample after the start can hold data from before it.** A sample
    ///   aggregates its step, and Prometheus' queries look back further, so the side a
    ///   sample falls on is where it is placed, not all of what it measured.
    /// - Coroot picks the step from the window's length: its cache step up to an hour
    ///   (whatever that step is), at least one minute up to six hours, five minutes up to
    ///   twelve and ten minutes up to the longest window, 24 hours.
    /// - A window ending after Coroot's newest data is cut there; see
    ///   [`RevisionCharts::ends_early`].
    /// - Coroot's 404 "Application not found" for the window is [`AroundRevision::NoData`],
    ///   not an error. It means the application has no metrics in the window, and it is
    ///   also what an application id that names no application gets, so `revision` must
    ///   be one read from `app` ([`Project::deployment_revisions`]). Nothing ties the two
    ///   together here. Any other 404 stays an error.
    ///
    /// An invalid `window` (a side under a minute or over twelve hours), invalid `limits`
    /// or an empty `app` fail with [`ErrorKind::InvalidInput`] before any request is sent.
    pub async fn charts_around_with(
        &self,
        app: &AppId,
        revision: &DeploymentRevision,
        window: RevisionWindow,
        limits: ChartLimits,
    ) -> Result<AroundRevision> {
        window.validate()?;
        let (from, to) = window.bounds(revision.started_at)?;
        match self
            .with_range(TimeRange::between(from, to))
            .app_charts_with(app, limits)
            .await
        {
            Ok(charts) => Ok(AroundRevision::Charts(RevisionCharts {
                revision: revision.id.clone(),
                started_at: revision.started_at,
                from,
                to,
                charts,
            })),
            Err(e) if AroundRevision::is_no_data(&e) => Ok(AroundRevision::NoData {
                revision: revision.id.clone(),
                from,
                to,
            }),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json;

    fn at(ms: i64) -> DateTime<Utc> {
        json::time_ms(ms).unwrap()
    }

    /// Seven places 30 s apart from 1790000010000 to 1790000190000.
    fn chart(samples: Vec<Option<f64>>, coverage: SeriesCoverage) -> ChartHistory {
        ChartHistory {
            group: None,
            title: "Requests, per second".into(),
            from: at(1790000010000),
            to: at(1790000190000),
            step: Duration::from_secs(30),
            truncated: false,
            stacked: false,
            series: vec![SeriesHistory {
                name: "ok".into(),
                title: String::new(),
                samples,
                coverage,
            }],
            threshold: None,
            annotations: Vec::new(),
        }
    }

    #[test]
    fn splits_places_at_the_first_at_or_after_the_instant() {
        let c = chart(vec![Some(1.0); 7], SeriesCoverage::Full);
        let split = |ms| c.split_at(at(ms));
        // On a place: that place is after.
        assert_eq!(
            split(1790000070000),
            ChartSplit {
                before: 2,
                after: 5
            }
        );
        // Between places: the next one is the first after.
        assert_eq!(
            split(1790000071000),
            ChartSplit {
                before: 3,
                after: 4
            }
        );
        // Before the window, after it.
        assert_eq!(
            split(1789999000000),
            ChartSplit {
                before: 0,
                after: 7
            }
        );
        assert_eq!(
            split(1790000999000),
            ChartSplit {
                before: 7,
                after: 0
            }
        );
    }

    #[test]
    fn sides_keep_gaps_and_unplaced_series_apart() {
        let s = ChartSplit {
            before: 3,
            after: 4,
        };
        let full = chart(
            vec![Some(1.0), None, Some(1.0), None, None, None, None],
            SeriesCoverage::Full,
        );
        assert_eq!(
            full.series[0].sides(&s),
            (
                Some(SideCoverage::Gaps {
                    present: 2,
                    expected: 3
                }),
                Some(SideCoverage::Empty)
            )
        );
        let all = chart(vec![Some(0.0); 7], SeriesCoverage::Full);
        assert_eq!(
            all.series[0].sides(&s),
            (Some(SideCoverage::Full), Some(SideCoverage::Full))
        );
        let short = chart(vec![Some(1.0); 2], SeriesCoverage::Partial);
        assert_eq!(
            short.series[0].sides(&s),
            (Some(SideCoverage::Unplaced), Some(SideCoverage::Unplaced))
        );
        let none = chart(Vec::new(), SeriesCoverage::Empty);
        assert_eq!(
            none.series[0].sides(&s),
            (Some(SideCoverage::Empty), Some(SideCoverage::Empty))
        );
        // A side without places has no coverage at all.
        let edge = ChartSplit {
            before: 0,
            after: 7,
        };
        assert_eq!(all.series[0].sides(&edge), (None, Some(SideCoverage::Full)));
        // A split from a longer chart runs past the samples: unplaced, not empty.
        let foreign = ChartSplit {
            before: 3,
            after: 9,
        };
        assert_eq!(
            all.series[0].sides(&foreign),
            (Some(SideCoverage::Full), Some(SideCoverage::Unplaced))
        );
        let huge = ChartSplit {
            before: 3,
            after: usize::MAX,
        };
        assert_eq!(
            all.series[0].sides(&huge),
            (Some(SideCoverage::Full), Some(SideCoverage::Unplaced))
        );
    }

    #[test]
    fn a_truncated_chart_splits_from_its_own_start() {
        // Coroot moved the window's start to 1790000100000, so the chart has four places
        // 30 s apart from there. A start at 1790000110000 has one of them before it.
        let mut c = chart(vec![Some(1.0); 4], SeriesCoverage::Full);
        c.from = at(1790000100000);
        c.truncated = true;
        assert_eq!(c.expected_points(), 4);
        assert_eq!(
            c.split_at(at(1790000110000)),
            ChartSplit {
                before: 1,
                after: 3
            }
        );
    }

    #[test]
    fn only_coroots_own_404_is_no_data() {
        let no_data = |kind, body: &str| AroundRevision::is_no_data(&Error::new(kind, body));
        assert!(no_data(ErrorKind::NotFound, "Application not found"));
        assert!(no_data(ErrorKind::NotFound, "Application not found\n"));
        assert!(!no_data(
            ErrorKind::NotFound,
            "<html><body>Not Found</body></html>"
        ));
        assert!(!no_data(ErrorKind::NotFound, "project not found"));
        assert!(!no_data(ErrorKind::Server, "Application not found"));
    }

    #[test]
    fn window_sides_are_bounded() {
        let ok = RevisionWindow::default();
        assert!(ok.validate().is_ok());
        assert_eq!(ok.before, Duration::from_secs(1800));
        for bad in [
            RevisionWindow::both(Duration::from_secs(59)),
            RevisionWindow::both(Duration::from_secs(12 * 3600 + 1)),
            RevisionWindow {
                before: Duration::ZERO,
                ..ok
            },
        ] {
            assert_eq!(bad.validate().unwrap_err().kind(), ErrorKind::InvalidInput);
        }
        let (from, to) = ok.bounds(at(1790000000000)).unwrap();
        assert_eq!(from.timestamp_millis(), 1790000000000 - 1_800_000);
        assert_eq!(to.timestamp_millis(), 1790000000000 + 1_800_000);
        // Coroot keeps whole seconds, so the bounds do too.
        let (from, to) = RevisionWindow::both(Duration::from_millis(90_500))
            .bounds(at(1790000000000))
            .unwrap();
        assert_eq!(from.timestamp_millis(), 1789999909000);
        assert_eq!(to.timestamp_millis(), 1790000090000);
        // The public range is the same window, and checked the same way.
        let range = ok.range(at(1790000000000)).unwrap();
        assert_eq!(range.from.unwrap().timestamp(), 1790000000 - 1800);
        assert_eq!(range.to.unwrap().timestamp(), 1790000000 + 1800);
        assert_eq!(
            RevisionWindow::both(Duration::from_secs(30))
                .range(at(1790000000000))
                .unwrap_err()
                .kind(),
            ErrorKind::InvalidInput
        );
    }

    #[test]
    fn ends_early_when_the_chart_stops_before_the_window() {
        let c = chart(vec![Some(1.0); 7], SeriesCoverage::Full);
        let around = |to_ms| RevisionCharts {
            revision: "7b8e21:1790000070".into(),
            started_at: at(1790000070000),
            from: at(1790000010000),
            to: at(to_ms),
            charts: AppCharts {
                app: AppId::new("c1:shop:Deployment:api"),
                reports: Vec::new(),
            },
        };
        assert!(!around(1790000190000).ends_early(&c));
        assert!(around(1790001870000).ends_early(&c));
        assert_eq!(
            around(1790000190000).split(&c),
            ChartSplit {
                before: 2,
                after: 5
            }
        );
    }
}
