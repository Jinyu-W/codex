use std::sync::OnceLock;

use codex_protocol::config_types::Personality;
use codex_protocol::config_types::ReasoningSummary;
use codex_protocol::openai_models::ConfigShellToolType;
use codex_protocol::openai_models::ApplyPatchToolType;
use codex_protocol::openai_models::ToolMode;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::openai_models::ModelMessages;
use codex_protocol::openai_models::ModelVisibility;
use codex_protocol::openai_models::TruncationMode;
use codex_protocol::openai_models::TruncationPolicyConfig;
use codex_protocol::openai_models::WebSearchToolType;
use codex_protocol::openai_models::default_input_modalities;

use crate::config::ModelsManagerConfig;
use codex_utils_output_truncation::approx_bytes_for_tokens;
use tracing::warn;

pub const BASE_INSTRUCTIONS: &str = include_str!("../prompt.md");
const PERSONALITY_SECTION_HEADER: &str = "# Personality";

pub fn with_config_overrides(mut model: ModelInfo, config: &ModelsManagerConfig) -> ModelInfo {
    if config.provider_parity {
        apply_provider_parity_profile(&mut model, config);
    }
    if let Some(context_window) = config.model_context_window {
        model.context_window = Some(
            model
                .max_context_window
                .map_or(context_window, |max_context_window| {
                    context_window.min(max_context_window)
                }),
        );
    }
    if let Some(auto_compact_token_limit) = config.model_auto_compact_token_limit {
        model.auto_compact_token_limit = Some(auto_compact_token_limit);
    }
    if let Some(token_limit) = config.tool_output_token_limit {
        model.truncation_policy = match model.truncation_policy.mode {
            TruncationMode::Bytes => {
                let byte_limit =
                    i64::try_from(approx_bytes_for_tokens(token_limit)).unwrap_or(i64::MAX);
                TruncationPolicyConfig::bytes(byte_limit)
            }
            TruncationMode::Tokens => {
                let limit = i64::try_from(token_limit).unwrap_or(i64::MAX);
                TruncationPolicyConfig::tokens(limit)
            }
        };
    }

    if let Some(base_instructions) = &config.base_instructions {
        let model_messages = model.model_messages.get_or_insert_default();
        model_messages.instructions_template = Some(base_instructions.clone());
    } else if config.personality == Some(Personality::None)
        && let Some(instructions_template) = model
            .model_messages
            .as_mut()
            .and_then(|messages| messages.instructions_template.as_mut())
    {
        *instructions_template = strip_personality_section(std::mem::take(instructions_template));
    }

    model
}

/// OpenAI catalog entry whose profile third-party slugs inherit.
const PARITY_REFERENCE_SLUG: &str = "gpt-6.1-sol";

/// Gives third-party provider models the exact tool surface OpenAI models get
/// from the model catalog: the `exec` orchestrator, structured `apply_patch`,
/// the `clock`/`sleep` nested tools, the multi-agent (`collaboration.*`) tools,
/// and OpenAI's tool-output truncation policy.
///
/// Hand-copying a few fields drifts as the catalog evolves (for example
/// `multi_agent_version` gates the `collaboration.*` tools and
/// `truncation_policy` was left at 10k *bytes* instead of OpenAI's 10k
/// *tokens*), so the profile is inherited from a reference catalog entry
/// instead.
fn apply_provider_parity_profile(model: &mut ModelInfo, config: &ModelsManagerConfig) {
    let from_config_catalog = config
        .model_catalog
        .as_ref()
        .and_then(|catalog| find_parity_reference(&catalog.models));
    if let Some(reference) = from_config_catalog {
        inherit_parity_profile(reference, model);
    } else if let Some(reference) = find_parity_reference(bundled_parity_catalog()) {
        inherit_parity_profile(reference, model);
    } else {
        apply_minimal_parity_profile(model);
    }
    // Responses Lite sends tools as an `additional_tools` input item, which
    // third-party gateways reject. Keep the classic top-level `tools` array.
    model.use_responses_lite = false;
}

/// Fields that describe the model/tool surface rather than anything
/// provider-specific. Context windows, auth, auth transport and pricing stay
/// untouched: those legitimately differ per endpoint.
fn inherit_parity_profile(reference: &ModelInfo, model: &mut ModelInfo) {
    model.tool_mode = reference.tool_mode;
    model.apply_patch_tool_type = reference.apply_patch_tool_type.clone();
    model.experimental_supported_tools = reference.experimental_supported_tools.clone();
    model.shell_type = reference.shell_type;
    model.web_search_tool_type = reference.web_search_tool_type;
    model.truncation_policy = reference.truncation_policy;
    model.multi_agent_version = reference.multi_agent_version;
    model.multi_agent_reasoning_effort = reference.multi_agent_reasoning_effort.clone();
    model.include_skills_usage_instructions = reference.include_skills_usage_instructions;
    model.include_plugin_usage_instructions = reference.include_plugin_usage_instructions;
    model.include_apps_usage_instructions = reference.include_apps_usage_instructions;
    model.supports_reasoning_summary_parameter = reference.supports_reasoning_summary_parameter;
    model.default_reasoning_summary = reference.default_reasoning_summary;
    model.support_verbosity = reference.support_verbosity;
    model.default_verbosity = reference.default_verbosity;
    model.supports_image_detail_original = reference.supports_image_detail_original;
    model.supports_reasoning_effort_updates = reference.supports_reasoning_effort_updates;
    model.supported_reasoning_levels = reference.supported_reasoning_levels.clone();
    model.default_reasoning_level = reference.default_reasoning_level.clone();
    model.input_modalities = reference.input_modalities.clone();
    // Deliberately not inherited: `supports_search_tool`. OpenAI models get it
    // because the OpenAI backend serves the hosted search tool; a third-party
    // gateway does not, and the flag *removes* direct tool exposure in favour of
    // the deferred/search path, so copying it would hide tools instead of
    // matching the surface.

    if reference.tool_mode.is_none() {
        warn!(
            "provider parity reference `{}` has no tool_mode; keeping fallback behaviour",
            reference.slug
        );
    }
}

/// Used only when no catalog entry is available (no catalog at all).
fn apply_minimal_parity_profile(model: &mut ModelInfo) {
    model.tool_mode = Some(ToolMode::CodeMode);
    if model.apply_patch_tool_type.is_none() {
        model.apply_patch_tool_type = Some(ApplyPatchToolType::Freeform);
    }
    if !model
        .experimental_supported_tools
        .iter()
        .any(|tool| tool == "clock")
    {
        model
            .experimental_supported_tools
            .push("clock".to_string());
    }
}

fn find_parity_reference(models: &[ModelInfo]) -> Option<&ModelInfo> {
    models
        .iter()
        .find(|model| model.slug == PARITY_REFERENCE_SLUG)
        .or_else(|| {
            models.iter().find(|model| {
                model.supported_in_api
                    && matches!(
                        model.tool_mode,
                        Some(ToolMode::CodeMode | ToolMode::CodeModeOnly)
                    )
            })
        })
}

fn bundled_parity_catalog() -> &'static [ModelInfo] {
    static CATALOG: OnceLock<Vec<ModelInfo>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        crate::bundled_models_response()
            .map(|response| response.models)
            .unwrap_or_default()
    })
}

fn strip_personality_section(mut instructions: String) -> String {
    let mut section_start = None;
    let mut section_end = None;
    let mut offset = 0;

    for line_with_ending in instructions.split_inclusive('\n') {
        let line = match line_with_ending.strip_suffix('\n') {
            Some(line) => line.strip_suffix('\r').unwrap_or(line),
            None => line_with_ending,
        };
        if section_start.is_some() {
            if is_h1_heading(line) {
                section_end = Some(offset);
                break;
            }
        } else if line == PERSONALITY_SECTION_HEADER {
            section_start = Some(offset);
        }
        offset += line_with_ending.len();
    }

    if let Some(section_start) = section_start {
        let section_end = section_end.unwrap_or(instructions.len());
        instructions.replace_range(section_start..section_end, "");
    }

    instructions
}

fn is_h1_heading(line: &str) -> bool {
    let Some(rest) = line.strip_prefix('#') else {
        return false;
    };
    rest.is_empty() || rest.starts_with(' ') || rest.starts_with('\t')
}

/// Build a minimal fallback model descriptor for missing/unknown slugs.
pub fn model_info_from_slug(slug: &str) -> ModelInfo {
    warn!("Unknown model {slug} is used. This will use fallback model metadata.");
    ModelInfo {
        slug: slug.to_string(),
        display_name: slug.to_string(),
        description: None,
        default_reasoning_level: None,
        supported_reasoning_levels: Vec::new(),
        shell_type: ConfigShellToolType::UnifiedExec,
        visibility: ModelVisibility::None,
        supported_in_api: true,
        priority: 99,
        additional_speed_tiers: Vec::new(),
        service_tiers: Vec::new(),
        default_service_tier: None,
        available_access_programs: None,
        availability_nux: None,
        upgrade: None,
        model_messages: Some(local_model_messages()),
        include_skills_usage_instructions: false,
        include_plugin_usage_instructions: false,
        include_apps_usage_instructions: false,
        supports_reasoning_summary_parameter: true,
        default_reasoning_summary: ReasoningSummary::Auto,
        support_verbosity: false,
        default_verbosity: None,
        apply_patch_tool_type: None,
        web_search_tool_type: WebSearchToolType::Text,
        truncation_policy: TruncationPolicyConfig::bytes(/*limit*/ 10_000),
        supports_image_detail_original: false,
        context_window: Some(272_000),
        max_context_window: Some(272_000),
        auto_compact_token_limit: None,
        comp_hash: None,
        effective_context_window_percent: 95,
        experimental_supported_tools: Vec::new(),
        input_modalities: default_input_modalities(),
        used_fallback_model_metadata: true, // this is the fallback model metadata
        supports_search_tool: false,
        supports_experimental_context: false,
        use_responses_lite: false,
        supports_reasoning_effort_updates: false,
        guardian: None,
        node_repl_auto_review_required: false,
        node_repl_disabled: false,
        auto_review_model_override: None,
        model_specialty: None,
        tool_mode: None,
        multi_agent_version: None,
        multi_agent_reasoning_effort: None,
    }
}

fn local_model_messages() -> ModelMessages {
    ModelMessages {
        instructions_template: Some(BASE_INSTRUCTIONS.to_string()),
        ..Default::default()
    }
}

#[cfg(test)]
#[path = "model_info_tests.rs"]
mod tests;
