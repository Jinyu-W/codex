use std::sync::Arc;

use chrono::DateTime;
use chrono::Utc;
use codex_features::CurrentTimeReminderDeliveryMode;
use codex_history::InitialHistory;
use codex_history::ResumedHistory;
use codex_history::RolloutItem;
use codex_protocol::ThreadId;
use codex_protocol::protocol::SessionMeta;
use codex_protocol::protocol::SessionMetaLine;

use super::*;
use crate::context::ContextualUserFragment;
use crate::context::CurrentTimeReminder;

fn utc(value: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(value)
        .expect("valid timestamp")
        .with_timezone(&Utc)
}

fn budget_config(
    deadline_epoch: Option<i64>,
    budget_seconds: Option<u64>,
    fractions: &[f64],
    convergence_hint: bool,
) -> SessionTimeBudgetConfig {
    SessionTimeBudgetConfig {
        deadline_epoch,
        budget_seconds,
        reminder_remaining_fractions: fractions.to_vec(),
        convergence_hint,
    }
}

fn thresholds(fractions: &[f64], limit: u64) -> Vec<SessionBudgetThreshold> {
    fractions
        .iter()
        .map(|fraction| SessionBudgetThreshold {
            fraction: *fraction,
            seconds: ((*fraction) * (limit as f64)).ceil() as i64,
        })
        .collect()
}

fn view_for(
    config: &SessionTimeBudgetConfig,
    started_at: DateTime<Utc>,
    now: DateTime<Utc>,
    inherited: bool,
) -> SessionBudgetView {
    let snapshot = compute_session_budget(config, started_at, now, inherited);
    SessionBudgetView {
        started_at: Some(started_at),
        elapsed_seconds: Some(now.signed_duration_since(started_at).num_seconds().max(0)),
        limit_seconds: snapshot.limit_seconds,
        remaining_seconds: snapshot.remaining_seconds,
        inherited_relative: snapshot.inherited_relative,
        threshold: None,
        convergence_hint: false,
    }
}

fn body(view: &SessionBudgetView, now: DateTime<Utc>) -> String {
    CurrentTimeReminder::with_session_budget(now, view.clone())
        .body()
        .into_text()
        .expect("text body")
}

fn offline_body(now: DateTime<Utc>) -> String {
    CurrentTimeReminder::new(now)
        .body()
        .into_text()
        .expect("text body")
}

// T1: elapsed/remaining arithmetic and formatting.
#[test]
fn t01_body_arithmetic() {
    let start = utc("2026-11-18T06:51:12Z");
    let now = utc("2026-11-18T07:22:03Z");
    let config = budget_config(None, Some(28_800), &[0.5, 0.25, 0.1], false);
    let text = body(&view_for(&config, start, now, false), now);
    assert_eq!(
        text,
        "It is 2026-11-18 07:22:03 UTC.\n\
Session start: 2026-11-18 06:51:12 UTC; elapsed: 00:30:51 (6% of the 8h 00m time limit).\n\
Remaining: 26,949 s (7h 29m)."
    );
}

// T2: a clock that moves backwards clamps elapsed to zero and remaining to the limit.
#[test]
fn t02_clock_backwards_clamps() {
    let start = utc("2026-11-18T06:00:00Z");
    let now = utc("2026-11-18T05:00:00Z");
    let config = budget_config(None, Some(28_800), &[], false);
    let view = view_for(&config, start, now, false);
    assert_eq!(view.elapsed_seconds, Some(0));
    assert_eq!(view.remaining_seconds, Some(28_800));
    assert!(body(&view, now).contains("elapsed: 00:00:00"));
    assert!(body(&view, now).contains("Remaining: 28,800 s (8h 00m)."));
}

// T3: when both keys are present the absolute deadline wins.
#[test]
fn t03_deadline_wins_over_budget_seconds() {
    let start = utc("2026-11-18T06:00:00Z");
    let now = utc("2026-11-18T07:00:00Z");
    let deadline = utc("2026-11-18T14:00:00Z").timestamp();
    let config = budget_config(Some(deadline), Some(60), &[], false);
    let view = view_for(&config, start, now, false);
    assert_eq!(view.limit_seconds, Some(28_800));
    assert_eq!(view.remaining_seconds, Some(25_200));
}

// T4: an absolute deadline still cannot report remaining when the start is unknown.
#[test]
fn t04_deadline_with_unknown_start_fails_closed() {
    let now = utc("2026-11-18T07:00:00Z");
    let view = SessionBudgetView {
        started_at: None,
        elapsed_seconds: None,
        limit_seconds: Some(28_800),
        remaining_seconds: Some(25_200),
        inherited_relative: false,
        threshold: None,
        convergence_hint: false,
    };
    assert_eq!(body(&view, now), "It is 2026-11-18 07:00:00 UTC.");
}

// T5: no start at all -> only the original single line, never a guessed remaining.
#[test]
fn t05_unknown_start_emits_only_first_line() {
    let start = utc("2026-11-18T06:00:00Z");
    let now = utc("2026-11-18T07:00:00Z");
    let config = budget_config(None, Some(3600), &[], false);
    let mut view = view_for(&config, start, now, false);
    view.started_at = None;
    view.elapsed_seconds = None;
    assert_eq!(body(&view, now), "It is 2026-11-18 07:00:00 UTC.");
}

// T7: crossing one threshold injects exactly once.
#[test]
fn t07_threshold_injected_once() {
    let thresholds = thresholds(&[0.5, 0.25, 0.1], 28_800);
    let mut state = CurrentTimeReminderState::default();
    let now = utc("2026-11-18T06:00:00Z");
    let _ = state.take_reminder_due(
        "w0",
        now,
        1_000_000,
        CurrentTimeReminderDeliveryMode::AnyInference,
        Some(20_000),
        &thresholds,
    );
    let first = state.take_reminder_due(
        "w0",
        now,
        1_000_000,
        CurrentTimeReminderDeliveryMode::AnyInference,
        Some(7_100),
        &thresholds,
    );
    assert_eq!(
        first,
        ReminderDue::Threshold(SessionBudgetThreshold {
            fraction: 0.25,
            seconds: 7_200
        })
    );
    let second = state.take_reminder_due(
        "w0",
        now,
        1_000_000,
        CurrentTimeReminderDeliveryMode::AnyInference,
        Some(7_100),
        &thresholds,
    );
    assert_eq!(second, ReminderDue::None);
}

// T8: one jump across several thresholds emits only the tightest and marks them all.
#[test]
fn t08_multi_threshold_jump_emits_tightest_once() {
    let thresholds = thresholds(&[0.5, 0.25, 0.1], 28_800);
    let mut state = CurrentTimeReminderState::default();
    let now = utc("2026-11-18T06:00:00Z");
    let _ = state.take_reminder_due(
        "w0",
        now,
        1_000_000,
        CurrentTimeReminderDeliveryMode::AnyInference,
        Some(20_000),
        &thresholds,
    );
    let due = state.take_reminder_due(
        "w0",
        now,
        1_000_000,
        CurrentTimeReminderDeliveryMode::AnyInference,
        Some(2_304),
        &thresholds,
    );
    assert_eq!(
        due,
        ReminderDue::Threshold(SessionBudgetThreshold {
            fraction: 0.1,
            seconds: 2_880
        })
    );
    assert_eq!(state.delivered_thresholds.len(), 3);
    let again = state.take_reminder_due(
        "w0",
        now,
        1_000_000,
        CurrentTimeReminderDeliveryMode::AnyInference,
        Some(2_000),
        &thresholds,
    );
    assert_eq!(again, ReminderDue::None);
}

// T9: a new window re-sends the regular reminder but never re-sends a threshold.
#[test]
fn t09_new_window_does_not_resend_thresholds() {
    let thresholds = thresholds(&[0.5, 0.25, 0.1], 28_800);
    let mut state = CurrentTimeReminderState::default();
    let now = utc("2026-11-18T06:00:00Z");
    let _ = state.take_reminder_due(
        "w0",
        now,
        1_000_000,
        CurrentTimeReminderDeliveryMode::AnyInference,
        Some(20_000),
        &thresholds,
    );
    let _ = state.take_reminder_due(
        "w0",
        now,
        1_000_000,
        CurrentTimeReminderDeliveryMode::AnyInference,
        Some(7_000),
        &thresholds,
    );
    let next_window = state.take_reminder_due(
        "w1",
        now,
        1_000_000,
        CurrentTimeReminderDeliveryMode::AnyInference,
        Some(6_900),
        &thresholds,
    );
    assert_eq!(next_window, ReminderDue::Interval);
}

// T10: enabled but no budget keys -> start/elapsed plus the canonical unknown line.
#[test]
fn t10_no_budget_emits_canonical_unknown_line() {
    let start = utc("2026-11-18T06:00:00Z");
    let now = utc("2026-11-18T06:10:00Z");
    let config = budget_config(None, None, &[], false);
    let text = body(&view_for(&config, start, now, false), now);
    assert_eq!(
        text,
        "It is 2026-11-18 06:10:00 UTC.\n\
Session start: 2026-11-18 06:00:00 UTC; elapsed: 00:10:00.\n\
Remaining: remaining unknown (no session budget configured)."
    );
    assert!(!text.contains('%'));
}

// T11: with the feature off the body is byte-identical to the baseline string.
#[test]
fn t11_disabled_body_is_byte_identical() {
    let now = utc("2026-11-18T06:10:00Z");
    let expected = format!(
        "It is {}.",
        now.format("%Y-%m-%d %H:%M:%S UTC")
    );
    assert_eq!(offline_body(now), expected);
    let empty_view = SessionBudgetView {
        started_at: None,
        elapsed_seconds: None,
        limit_seconds: None,
        remaining_seconds: None,
        inherited_relative: false,
        threshold: None,
        convergence_hint: false,
    };
    assert_eq!(
        crate::context::render_reminder_body("2026-11-18 06:10:00 UTC", &empty_view),
        expected
    );
}

// T13: a resumed session takes its elapsed base from the canonical SessionMeta.
#[test]
fn t13_resume_uses_canonical_session_meta_timestamp() {
    let thread_id = ThreadId::default();
    let mut meta = SessionMeta {
        id: thread_id,
        session_id: thread_id.into(),
        timestamp: "2026-11-18T06:00:00Z".to_string(),
        ..SessionMeta::default()
    };
    let history = vec![RolloutItem::SessionMeta(Box::new(SessionMetaLine {
        meta: meta.clone(),
        git: None,
    }))];
    let resumed = InitialHistory::Resumed(ResumedHistory {
        conversation_id: thread_id,
        history: Arc::new(history),
        history_revision: None,
        rollout_path: None,
    });
    assert_eq!(
        crate::session::session::initial_session_start(&resumed),
        SessionStart::Known(utc("2026-11-18T06:00:00Z"))
    );

    meta.timestamp = "not-a-timestamp".to_string();
    let broken = InitialHistory::Resumed(ResumedHistory {
        conversation_id: thread_id,
        history: Arc::new(vec![RolloutItem::SessionMeta(Box::new(SessionMetaLine {
            meta,
            git: None,
        }))]),
        history_revision: None,
        rollout_path: None,
    });
    assert_eq!(
        crate::session::session::initial_session_start(&broken),
        SessionStart::Unavailable
    );

    let new_session = InitialHistory::New;
    assert_eq!(
        crate::session::session::initial_session_start(&new_session),
        SessionStart::Unresolved
    );
}

// T14: a fork inherits the same absolute deadline, independent of its own start.
#[test]
fn t14_fork_inherits_absolute_deadline() {
    let parent_start = utc("2026-11-18T06:00:00Z");
    let fork_start = utc("2026-11-18T07:00:00Z");
    let now = utc("2026-11-18T07:00:00Z");
    let deadline = utc("2026-11-18T14:00:00Z").timestamp();
    let config = budget_config(Some(deadline), None, &[], false);
    let parent = view_for(&config, parent_start, now, false);
    let child = view_for(&config, fork_start, now, false);
    assert_eq!(child.remaining_seconds, parent.remaining_seconds);
    assert_eq!(child.remaining_seconds, Some(25_200));
}

// T15: a child that inherited only a relative budget reports fork time and
// elapsed, never remaining, and marks the inheritance.
#[test]
fn t15_fork_relative_budget_reports_start_and_elapsed_only() {
    let fork_start = utc("2026-11-18T07:00:00Z");
    let now = utc("2026-11-18T07:05:00Z");
    let config = budget_config(None, Some(28_800), &[0.5], false);
    let view = view_for(&config, fork_start, now, true);
    assert_eq!(view.limit_seconds, None);
    assert_eq!(view.remaining_seconds, None);
    assert!(view.inherited_relative);
    let text = body(&view, now);
    assert_eq!(
        text,
        "It is 2026-11-18 07:05:00 UTC.\n\
Session start: 2026-11-18 07:00:00 UTC; elapsed: 00:05:00 (fork; inherited: no)."
    );
    assert!(!text.contains("Remaining:"));
}

// T16: the convergence hint is off by default and appears only when enabled.
#[test]
fn t16_convergence_hint_switch() {
    let start = utc("2026-11-18T06:00:00Z");
    let now = utc("2026-11-18T13:45:00Z");
    let config = budget_config(None, Some(28_800), &[0.1], true);
    let mut view = view_for(&config, start, now, false);
    view.threshold = Some(SessionBudgetThreshold {
        fraction: 0.1,
        seconds: 2_880,
    });
    view.convergence_hint = true;
    let enabled = body(&view, now);
    assert!(enabled.contains(
        "Ensure complete deliverables are on disk and state the source of the validation evidence."
    ));
    view.convergence_hint = false;
    let disabled = body(&view, now);
    assert!(!disabled.contains("Ensure complete deliverables"));
}

// T17: a backwards clock never panics and clamps both elapsed and remaining.
#[test]
fn t17_backwards_clock_no_panic() {
    let start = utc("2026-11-18T06:00:00Z");
    let now = utc("2026-11-18T05:59:00Z");
    let config = budget_config(None, Some(60), &[0.5], false);
    let view = view_for(&config, start, now, false);
    assert_eq!(view.elapsed_seconds, Some(0));
    assert_eq!(view.remaining_seconds, Some(60));
}

// T18 (D-5): an unknown resumed start stays unknown even after reading the clock.
#[test]
fn t18_resume_without_timestamp_never_guesses() {
    let now = utc("2026-11-18T07:00:00Z");
    let mut state = CurrentTimeReminderState::default();
    state.initialize_session_start(SessionStart::Unavailable);
    assert_eq!(state.resolve_session_start(now), None);
    assert_eq!(state.session_start, SessionStart::Unavailable);
    assert_eq!(state.resolve_session_start(utc("2026-11-18T08:00:00Z")), None);
}

// T19 (D-6): at process start already-crossed thresholds are marked silently;
// a threshold crossed later still fires exactly once.
#[test]
fn t19_resume_marks_crossed_thresholds_silently() {
    let thresholds = thresholds(&[0.5, 0.25, 0.1], 28_800);
    let mut state = CurrentTimeReminderState::default();
    state.initialize_thresholds(Some(8_000), &thresholds);
    assert_eq!(state.delivered_thresholds.iter().copied().collect::<Vec<_>>(), vec![14_400]);

    let now = utc("2026-11-18T06:00:00Z");
    let first = state.take_reminder_due(
        "w0",
        now,
        1_000_000,
        CurrentTimeReminderDeliveryMode::AnyInference,
        Some(8_000),
        &thresholds,
    );
    assert_eq!(first, ReminderDue::Interval);
    let later = state.take_reminder_due(
        "w0",
        now,
        1_000_000,
        CurrentTimeReminderDeliveryMode::AnyInference,
        Some(7_100),
        &thresholds,
    );
    assert_eq!(
        later,
        ReminderDue::Threshold(SessionBudgetThreshold {
            fraction: 0.25,
            seconds: 7_200
        })
    );
}

// T20: a child with no absolute deadline never injects thresholds, even at zero remaining.
#[test]
fn t20_fork_relative_budget_never_injects_thresholds() {
    let fork_start = utc("2026-11-18T07:00:00Z");
    let now = utc("2026-11-18T09:00:00Z");
    let config = budget_config(None, Some(28_800), &[0.5, 0.25, 0.1], true);
    let snapshot = compute_session_budget(&config, fork_start, now, true);
    assert_eq!(snapshot.remaining_seconds, None);
    assert!(snapshot.thresholds.is_empty());
    assert!(snapshot.inherited_relative);
}

// A threshold body keeps the exact configured fraction and seconds.
#[test]
fn threshold_line_shape() {
    let start = utc("2026-11-18T06:00:00Z");
    let now = utc("2026-11-18T12:00:00Z");
    let config = budget_config(None, Some(28_800), &[0.25], false);
    let mut view = view_for(&config, start, now, false);
    view.threshold = Some(SessionBudgetThreshold {
        fraction: 0.25,
        seconds: 7_200,
    });
    assert!(body(&view, now).contains("\nSession budget threshold: 25% remaining (7,200 s)."));
}

// Fixture capture for the review package: off / A0 / A1 bodies under fixed
// inputs. Run with `--nocapture`; markers make the output easy to extract.
#[test]
fn fixture_capture_off_a0_a1() {
    let now = utc("2026-11-18T07:22:03Z");
    let start = utc("2026-11-18T06:51:12Z");
    println!("FIXTURE-BEGIN off");
    println!("{}", offline_body(now));
    println!("FIXTURE-END off");

    let a0 = budget_config(None, Some(28_800), &[], false);
    println!("FIXTURE-BEGIN a0");
    println!("{}", body(&view_for(&a0, start, now, false), now));
    println!("FIXTURE-END a0");

    let late_now = utc("2026-11-18T13:45:00Z");
    let a1 = budget_config(None, Some(28_800), &[0.5, 0.25, 0.1], false);
    let mut view = view_for(&a1, start, late_now, false);
    view.threshold = Some(SessionBudgetThreshold {
        fraction: 0.1,
        seconds: 2_880,
    });
    println!("FIXTURE-BEGIN a1");
    println!("{}", body(&view, late_now));
    println!("FIXTURE-END a1");
}
