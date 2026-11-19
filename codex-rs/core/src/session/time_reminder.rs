use std::collections::BTreeSet;

use chrono::DateTime;
use chrono::Utc;
use codex_features::CurrentTimeReminderDeliveryMode;
use codex_features::Feature;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::models::ResponseItem;

use super::session::Session;
use super::turn_context::TurnContext;
use crate::config::Config;
use crate::config::CurrentTimeReminderConfig;
use crate::config::SessionTimeBudgetConfig;
use crate::context::SessionBudgetThreshold;
use crate::context::SessionBudgetView;
use crate::context::ContextualUserFragment;
use crate::context::CurrentTimeUnavailable;
use crate::context_manager::is_user_turn_boundary;

pub(super) fn apply_persistent_defaults(config: &mut Config) {
    if config.current_time_reminder.is_some()
        || config
            .config_layer_stack
            .effective_config()
            .get("features")
            .and_then(|features| features.get("current_time_reminder"))
            .is_some()
    {
        return;
    }

    // Apply defaults only to this turn; explicit settings and managed policy win.
    if config.features.enable(Feature::CurrentTimeReminder).is_ok()
        && config.features.enabled(Feature::CurrentTimeReminder)
    {
        config.current_time_reminder = Some(CurrentTimeReminderConfig {
            sleep_tool: true,
            ..CurrentTimeReminderConfig::default()
        });
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum SessionStart {
    /// A new (or same-process forked) session: the first observed clock reading
    /// initializes the start.
    #[default]
    Unresolved,
    Known(DateTime<Utc>),
    /// A resumed session whose canonical `SessionMeta.timestamp` is missing or
    /// unparsable. Fail closed: never substitute the current time.
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum ReminderDue {
    None,
    Interval,
    Threshold(SessionBudgetThreshold),
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SessionBudgetSnapshot {
    pub limit_seconds: Option<u64>,
    pub remaining_seconds: Option<i64>,
    pub inherited_relative: bool,
    pub thresholds: Vec<SessionBudgetThreshold>,
}

/// Derive the wall-clock facts for one reminder from configured inputs.
///
/// All clamps live here: elapsed >= 0; remaining within `0..=limit`; a child
/// agent that inherited a relative-only budget reports no limit and no
/// remaining time, so it cannot restart the parent's budget.
pub(crate) fn compute_session_budget(
    config: &SessionTimeBudgetConfig,
    started_at: DateTime<Utc>,
    now: DateTime<Utc>,
    inherited_relative_budget: bool,
) -> SessionBudgetSnapshot {
    let elapsed = now
        .signed_duration_since(started_at)
        .num_seconds()
        .max(0);
    let mut limit_seconds = None;
    let mut remaining_seconds = None;
    let mut inherited_relative = false;

    if let Some(deadline_epoch) = config.deadline_epoch {
        let limit = deadline_epoch
            .saturating_sub(started_at.timestamp())
            .max(0);
        let remaining = deadline_epoch
            .saturating_sub(now.timestamp())
            .clamp(0, limit);
        limit_seconds = Some(u64::try_from(limit).unwrap_or_default());
        remaining_seconds = Some(remaining);
    } else if let Some(budget_seconds) = config.budget_seconds {
        if inherited_relative_budget {
            inherited_relative = true;
        } else {
            let limit = i64::try_from(budget_seconds).unwrap_or(i64::MAX);
            limit_seconds = Some(budget_seconds);
            remaining_seconds = Some(limit.saturating_sub(elapsed).clamp(0, limit));
        }
    }

    let thresholds = match limit_seconds {
        Some(limit) => config
            .reminder_remaining_fractions
            .iter()
            .filter_map(|fraction| {
                let seconds = ((*fraction) * (limit as f64)).ceil() as i64;
                (seconds > 0).then_some(SessionBudgetThreshold {
                    fraction: *fraction,
                    seconds,
                })
            })
            .collect(),
        None => Vec::new(),
    };

    SessionBudgetSnapshot {
        limit_seconds,
        remaining_seconds,
        inherited_relative,
        thresholds,
    }
}

#[derive(Default)]
pub(crate) struct CurrentTimeReminderState {
    last_delivery_time: Option<DateTime<Utc>>,
    last_window_id: Option<String>,
    last_clock_failure: Option<(String, uuid::Uuid)>,
    pending_user_or_tool_output_boundary: bool,
    session_start: SessionStart,
    /// Session-scoped, process-local. Deliberately not cleared on compaction.
    delivered_thresholds: BTreeSet<i64>,
    thresholds_initialized: bool,
}

impl CurrentTimeReminderState {
    pub(crate) fn initialize_session_start(&mut self, session_start: SessionStart) {
        self.session_start = session_start;
    }

    fn resolve_session_start(&mut self, current_time: DateTime<Utc>) -> Option<DateTime<Utc>> {
        match self.session_start {
            SessionStart::Known(started_at) => Some(started_at),
            SessionStart::Unresolved => {
                self.session_start = SessionStart::Known(current_time);
                Some(current_time)
            }
            SessionStart::Unavailable => None,
        }
    }

    /// Process-start threshold initialization: thresholds already crossed when
    /// this runtime starts (including resume) are marked delivered without
    /// injecting anything and without touching `last_delivery_time`.
    fn initialize_thresholds(
        &mut self,
        remaining_seconds: Option<i64>,
        thresholds: &[SessionBudgetThreshold],
    ) {
        if self.thresholds_initialized {
            return;
        }
        self.thresholds_initialized = true;
        let Some(remaining_seconds) = remaining_seconds else {
            return;
        };
        for threshold in thresholds {
            if remaining_seconds <= threshold.seconds {
                self.delivered_thresholds.insert(threshold.seconds);
            }
        }
    }

    pub(super) fn note_recorded_items(&mut self, items: &[ResponseItem]) {
        if items.iter().any(|item| {
            is_user_turn_boundary(item)
                || matches!(
                    item,
                    ResponseItem::FunctionCallOutput { .. }
                        | ResponseItem::CustomToolCallOutput { .. }
                        | ResponseItem::ToolSearchOutput { .. }
                )
        }) {
            self.pending_user_or_tool_output_boundary = true;
        }
    }

    fn take_reminder_due(
        &mut self,
        window_id: &str,
        current_time: DateTime<Utc>,
        interval_seconds: u64,
        delivery_mode: CurrentTimeReminderDeliveryMode,
        remaining_seconds: Option<i64>,
        thresholds: &[SessionBudgetThreshold],
    ) -> ReminderDue {
        let is_new_window = self.last_window_id.as_deref() != Some(window_id);
        // Consume the boundary for this inference even if the interval suppresses delivery.
        let follows_user_or_tool_output =
            std::mem::take(&mut self.pending_user_or_tool_output_boundary);
        let at_boundary = delivery_mode == CurrentTimeReminderDeliveryMode::AnyInference
            || is_new_window
            || follows_user_or_tool_output;
        if !at_boundary {
            return ReminderDue::None;
        }

        // Thresholds are progress signals: they bypass the interval but obey the
        // same delivery-boundary rule as the regular reminder.
        if let Some(remaining_seconds) = remaining_seconds {
            let mut crossed: Vec<SessionBudgetThreshold> = thresholds
                .iter()
                .copied()
                .filter(|threshold| {
                    remaining_seconds <= threshold.seconds
                        && !self.delivered_thresholds.contains(&threshold.seconds)
                })
                .collect();
            if !crossed.is_empty() {
                crossed.sort_by_key(|threshold| threshold.seconds);
                let tightest = crossed[0];
                for threshold in crossed {
                    self.delivered_thresholds.insert(threshold.seconds);
                }
                self.last_delivery_time = Some(current_time);
                self.last_window_id = Some(window_id.to_string());
                return ReminderDue::Threshold(tightest);
            }
        }

        let reminder_is_due = is_new_window
            || interval_seconds == 0
            || self.last_delivery_time.is_none_or(|last_delivery_time| {
                current_time
                    .signed_duration_since(last_delivery_time)
                    .num_seconds()
                    >= i64::try_from(interval_seconds).unwrap_or(i64::MAX)
            });

        if reminder_is_due {
            self.last_delivery_time = Some(current_time);
            self.last_window_id = Some(window_id.to_string());
            return ReminderDue::Interval;
        }

        ReminderDue::None
    }
}

impl Session {
    pub(super) async fn read_clock_for_context(
        &self,
        turn_context: &TurnContext,
        clock_read: &'static str,
    ) -> CodexResult<Option<DateTime<Utc>>> {
        let error = match self
            .services
            .time_provider
            .current_time(self.thread_id)
            .await
        {
            Ok(time) => {
                let mut state = self.state.lock().await;
                state.current_time_reminder.last_clock_failure = None;
                return Ok(Some(time));
            }
            Err(error) => error,
        };
        if !turn_context
            .config
            .features
            .enabled(Feature::NonfatalClockReadErrors)
        {
            return Err(CodexErr::Fatal(format!(
                "failed to read current time: {error:#}"
            )));
        }
        tracing::error!(
            clock_read,
            thread_id = %self.thread_id,
            turn_id = %turn_context.sub_id,
            "failed to read current time; the clock provider may be stalled"
        );
        {
            let mut state = self.state.lock().await;
            let failure = (
                turn_context.sub_id.clone(),
                state.auto_compact_window_ids().window_id,
            );
            // A compacted window may no longer contain the earlier notice.
            if state.current_time_reminder.last_clock_failure.as_ref() == Some(&failure) {
                return Ok(None);
            }
            state.current_time_reminder.last_clock_failure = Some(failure);
        }
        let response_item = ContextualUserFragment::into(CurrentTimeUnavailable);
        self.record_conversation_items(
            turn_context,
            turn_context.model_info(),
            std::slice::from_ref(&response_item),
        )
        .await;
        Ok(None)
    }
}

pub(super) async fn maybe_record_current_time_reminder(
    sess: &Session,
    turn_context: &TurnContext,
    window_id: &str,
) -> CodexResult<()> {
    if !turn_context
        .config
        .features
        .enabled(Feature::CurrentTimeReminder)
    {
        return Ok(());
    }
    let Some(config) = turn_context.config.current_time_reminder else {
        return Ok(());
    };

    let Some(current_time) = sess
        .read_clock_for_context(turn_context, "reminder")
        .await?
    else {
        return Ok(());
    };

    let budget_config = turn_context.config.session_time_budget.clone();
    let inherited_relative_budget = turn_context.session_source.is_non_root_agent();

    let (reminder_due, budget_view) = {
        let mut state = sess.state.lock().await;
        let started_at = state
            .current_time_reminder
            .resolve_session_start(current_time);

        let elapsed_seconds = started_at
            .map(|started_at| current_time.signed_duration_since(started_at).num_seconds().max(0));
        let snapshot = budget_config.as_ref().zip(started_at).map(
            |(budget_config, started_at)| {
                compute_session_budget(
                    budget_config,
                    started_at,
                    current_time,
                    inherited_relative_budget,
                )
            },
        );
        let limit_seconds = snapshot.as_ref().and_then(|snapshot| snapshot.limit_seconds);
        let remaining_seconds = snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.remaining_seconds);
        let inherited_relative = snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.inherited_relative);
        let thresholds = snapshot
            .as_ref()
            .map(|snapshot| snapshot.thresholds.clone())
            .unwrap_or_default();

        state
            .current_time_reminder
            .initialize_thresholds(remaining_seconds, &thresholds);

        let reminder_due = state.current_time_reminder.take_reminder_due(
            window_id,
            current_time,
            config.reminder_interval_seconds,
            config.delivery_mode,
            remaining_seconds,
            &thresholds,
        );

        let budget_view = if budget_config.is_some() && reminder_due != ReminderDue::None {
            let threshold = match reminder_due {
                ReminderDue::Threshold(threshold) => Some(threshold),
                ReminderDue::None | ReminderDue::Interval => None,
            };
            let convergence_hint = budget_config.as_ref().is_some_and(|budget_config| {
                budget_config.convergence_hint
                    && threshold.is_some_and(|threshold| {
                        thresholds
                            .iter()
                            .map(|configured| configured.seconds)
                            .min()
                            == Some(threshold.seconds)
                    })
            });
            Some(SessionBudgetView {
                started_at,
                elapsed_seconds,
                limit_seconds,
                remaining_seconds,
                inherited_relative,
                threshold,
                convergence_hint,
            })
        } else {
            None
        };

        (reminder_due, budget_view)
    };
    if reminder_due == ReminderDue::None {
        return Ok(());
    }

    let response_item = match budget_view {
        Some(view) => {
            ContextualUserFragment::into(crate::context::CurrentTimeReminder::with_session_budget(
                current_time, view,
            ))
        }
        None => ContextualUserFragment::into(crate::context::CurrentTimeReminder::new(current_time)),
    };
    sess.record_conversation_items(
        turn_context,
        turn_context.model_info(),
        std::slice::from_ref(&response_item),
    )
    .await;

    Ok(())
}

#[cfg(test)]
#[path = "time_reminder_tests.rs"]
mod tests;
