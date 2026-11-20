//! Pre-delivery self-check instruction.
//!
//! When [`codex_features::Feature::DeliverySelfCheck`] is enabled, a single
//! generic paragraph is appended to the developer instructions: the agent is
//! asked to record, in one line, what evidence it used to judge that the
//! deliverable works and where that evidence came from. The line is a record
//! for later inspection; it is not an acceptance check and does not gate the
//! turn.

/// Generic instruction text. Kept free of task, benchmark, and path literals.
pub(crate) const DELIVERY_SELF_CHECK_INSTRUCTIONS: &str = "Before finalizing, write one line starting with `validation_note:` that states what evidence you used to judge that the deliverable works, and where that evidence came from: data with a known answer (constructed, or held out in the same structure as the stated acceptance criteria), or goodness-of-fit on the data used to build it. If the only evidence is self-consistency, say so explicitly. This line records how the work was checked; it does not replace any acceptance check.";

/// Build the developer instructions that will be shown to the model.
///
/// With the feature disabled the input is returned unchanged, so the rendered
/// context is byte-identical to a session without this feature. With the
/// feature enabled the paragraph is appended after a blank line; when there
/// are no existing instructions the paragraph is used on its own.
pub(crate) fn compose_developer_instructions_with_self_check(
    existing: Option<&str>,
    enabled: bool,
) -> Option<String> {
    if !enabled {
        return existing.map(str::to_string);
    }

    match existing {
        Some(text) if !text.is_empty() => {
            Some(format!("{text}\n\n{DELIVERY_SELF_CHECK_INSTRUCTIONS}"))
        }
        _ => Some(DELIVERY_SELF_CHECK_INSTRUCTIONS.to_string()),
    }
}

#[cfg(test)]
#[path = "delivery_self_check_tests.rs"]
mod tests;
