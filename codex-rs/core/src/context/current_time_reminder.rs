use chrono::DateTime;
use chrono::Utc;

use super::ContextualUserFragment;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ContentItemKind;
use codex_protocol::models::ContentItemMetadata;
use codex_protocol::models::ContentItemNamespace;
use codex_protocol::models::input_text;

/// A threshold reminder that is being delivered together with this body.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct SessionBudgetThreshold {
    /// Configured remaining-time fraction, e.g. `0.25`.
    pub fraction: f64,
    /// Threshold in remaining seconds, e.g. `7200`.
    pub seconds: i64,
}

/// Caller-supplied wall-clock facts for one reminder body.
///
/// Everything here is derived from the runtime clock and the caller's config.
/// No values are guessed: when a field cannot be computed it stays `None` and
/// the corresponding line is omitted.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SessionBudgetView {
    /// Session start. `None` means unknown; the body then keeps only the
    /// original `It is ...` line.
    pub started_at: Option<DateTime<Utc>>,
    /// Whole seconds since `started_at`, clamped to >= 0.
    pub elapsed_seconds: Option<i64>,
    /// Total budget in seconds, when derivable.
    pub limit_seconds: Option<u64>,
    /// Remaining seconds, clamped to `0..=limit`.
    pub remaining_seconds: Option<i64>,
    /// True for a child agent that inherited a relative-only budget: it reports
    /// its own fork time and elapsed time, but no remaining time and no
    /// thresholds.
    pub inherited_relative: bool,
    /// Threshold being delivered on this boundary, if any.
    pub threshold: Option<SessionBudgetThreshold>,
    /// Whether to append the convergence hint on this boundary.
    pub convergence_hint: bool,
}

pub(crate) struct CurrentTimeReminder {
    current_time: DateTime<Utc>,
    session_budget: Option<SessionBudgetView>,
}

impl CurrentTimeReminder {
    pub(crate) fn new(current_time: DateTime<Utc>) -> Self {
        Self {
            current_time,
            session_budget: None,
        }
    }

    pub(crate) fn with_session_budget(
        current_time: DateTime<Utc>,
        session_budget: SessionBudgetView,
    ) -> Self {
        Self {
            current_time,
            session_budget: Some(session_budget),
        }
    }

    pub(crate) fn formatted_time(&self) -> String {
        self.current_time
            .format("%Y-%m-%d %H:%M:%S UTC")
            .to_string()
    }
}

impl ContextualUserFragment for CurrentTimeReminder {
    fn content_metadata(&self) -> ContentItemMetadata {
        ContentItemMetadata::tool(Some(ContentItemNamespace::Clock))
    }

    fn content_kind(&self) -> ContentItemKind {
        ContentItemKind("current_time.reminder".to_string())
    }

    fn role(&self) -> &'static str {
        "developer"
    }

    fn markers(&self) -> (&'static str, &'static str) {
        Self::type_markers()
    }

    fn type_markers() -> (&'static str, &'static str) {
        ("<current_time_reminder>", "</current_time_reminder>")
    }

    fn body(&self) -> ContentItem {
        let now = self.formatted_time();
        let body = match &self.session_budget {
            None => format!("It is {now}."),
            Some(view) => render_reminder_body(&now, view),
        };
        input_text(body)
    }
}

/// Render the reminder body. With no budget view (or an unknown start) this is
/// byte-for-byte the original single-line reminder.
pub(crate) fn render_reminder_body(now: &str, view: &SessionBudgetView) -> String {
    let mut body = format!("It is {now}.");
    let Some(started_at) = view.started_at else {
        return body;
    };
    let started_at = started_at.format("%Y-%m-%d %H:%M:%S UTC").to_string();
    let elapsed = view.elapsed_seconds.unwrap_or_default().max(0);
    body.push_str("\nSession start: ");
    body.push_str(&started_at);
    body.push_str("; elapsed: ");
    body.push_str(&format_elapsed(elapsed));
    if let (Some(limit), true) = (view.limit_seconds, !view.inherited_relative) {
        body.push_str(&format!(
            " ({}% of the {} time limit)",
            elapsed_percent(elapsed, limit),
            format_duration(limit)
        ));
    }
    if view.inherited_relative {
        body.push_str(" (fork; inherited: no)");
    }
    body.push('.');
    if let Some(remaining) = view.remaining_seconds {
        body.push_str("\nRemaining: ");
        body.push_str(&format_remaining(remaining));
        body.push('.');
    } else if !view.inherited_relative {
        body.push_str("\nRemaining: remaining unknown (no session budget configured).");
    }
    if let Some(threshold) = view.threshold {
        body.push_str(&format!(
            "\nSession budget threshold: {}% remaining ({} s).",
            format_fraction_percent(threshold.fraction),
            format_thousands(threshold.seconds)
        ));
    }
    if view.convergence_hint {
        body.push_str(
            "\nEnsure complete deliverables are on disk and state the source of the validation evidence.",
        );
    }
    body
}

fn elapsed_percent(elapsed_seconds: i64, limit_seconds: u64) -> u64 {
    if limit_seconds == 0 {
        return 0;
    }
    let used = u64::try_from(elapsed_seconds.max(0)).unwrap_or(u64::MAX);
    used.min(limit_seconds).saturating_mul(100) / limit_seconds
}

fn format_elapsed(seconds: i64) -> String {
    let seconds = u64::try_from(seconds.max(0)).unwrap_or(0);
    format!(
        "{:02}:{:02}:{:02}",
        seconds / 3600,
        (seconds % 3600) / 60,
        seconds % 60
    )
}

fn format_duration(seconds: u64) -> String {
    if seconds >= 3600 {
        format!("{}h {:02}m", seconds / 3600, (seconds % 3600) / 60)
    } else if seconds >= 60 {
        format!("{}m {:02}s", seconds / 60, seconds % 60)
    } else {
        format!("{seconds}s")
    }
}

fn format_remaining(seconds: i64) -> String {
    let seconds = seconds.max(0);
    format!(
        "{} s ({})",
        format_thousands(seconds),
        format_duration(u64::try_from(seconds).unwrap_or_default())
    )
}

fn format_fraction_percent(fraction: f64) -> String {
    let percent = fraction * 100.0;
    let rounded = (percent * 1000.0).round() / 1000.0;
    if (rounded - rounded.trunc()).abs() < f64::EPSILON {
        format!("{}", rounded.trunc() as i64)
    } else {
        let text = format!("{rounded:.3}");
        text.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

pub(crate) fn format_thousands(value: i64) -> String {
    let negative = value < 0;
    let digits = value.unsigned_abs().to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3 + 1);
    if negative {
        out.push('-');
    }
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

pub(crate) struct CurrentTimeUnavailable;

impl CurrentTimeUnavailable {
    pub(crate) const MESSAGE: &str = "failed to read current time";
}

impl ContextualUserFragment for CurrentTimeUnavailable {
    fn content_metadata(&self) -> ContentItemMetadata {
        ContentItemMetadata::tool(Some(ContentItemNamespace::Clock))
    }

    fn content_kind(&self) -> ContentItemKind {
        ContentItemKind("current_time.unavailable".to_string())
    }

    fn role(&self) -> &'static str {
        "developer"
    }

    fn markers(&self) -> (&'static str, &'static str) {
        Self::type_markers()
    }

    fn type_markers() -> (&'static str, &'static str) {
        ("<current_time_unavailable>", "</current_time_unavailable>")
    }

    fn body(&self) -> ContentItem {
        input_text(Self::MESSAGE.to_string())
    }
}
