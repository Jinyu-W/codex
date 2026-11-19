//! Keeps the OpenAI-provider tool surface when Codex targets a third-party
//! OpenAI-compatible endpoint.
//!
//! The tool surface is driven by OpenAI model-catalog metadata (`tool_mode`,
//! `apply_patch_tool_type`, `experimental_supported_tools`), which does not
//! exist for third-party model slugs. Without this profile those sessions fall
//! back to `ToolMode::Direct`: no `exec` orchestrator, no structured
//! `apply_patch`, no `clock`/`sleep`.

use codex_protocol::models::plaintext_agent_message_content;
use codex_protocol::models::ContentItem;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputContentItem;
use codex_protocol::models::ResponseItem;

/// How free-form (`custom`) tool formats are sent to the endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum FreeformToolFormatMode {
    #[default]
    Auto,
    Grammar,
    Text,
}

/// How the `developer` role is sent to the endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum DeveloperRoleMode {
    #[default]
    Auto,
    Developer,
    System,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProviderParityConfig {
    pub enabled: bool,
    pub freeform_tool_format: FreeformToolFormatMode,
    pub developer_role: DeveloperRoleMode,
}

impl Default for ProviderParityConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            freeform_tool_format: FreeformToolFormatMode::Auto,
            developer_role: DeveloperRoleMode::Auto,
        }
    }
}

impl ProviderParityConfig {
    /// Whether free-form tools must be downgraded from `grammar` to `text`.
    pub(crate) fn downgrade_freeform_to_text(self) -> bool {
        match self.freeform_tool_format {
            FreeformToolFormatMode::Auto => self.enabled,
            FreeformToolFormatMode::Text => true,
            FreeformToolFormatMode::Grammar => false,
        }
    }

    /// Whether multi-agent `agent_message` input items must be rewritten to
    /// plain assistant `message` items.
    pub(crate) fn rewrite_agent_messages(self) -> bool {
        self.enabled
    }

    /// Whether `developer`-role messages must be rewritten to `system`.
    pub(crate) fn rewrite_developer_as_system(self) -> bool {
        match self.developer_role {
            DeveloperRoleMode::Auto => self.enabled,
            DeveloperRoleMode::System => true,
            DeveloperRoleMode::Developer => false,
        }
    }
}

#[cfg(test)]
#[path = "provider_parity_tests.rs"]
mod tests;

/// Rewrites tool outputs from the content-item array form to a plain string.
///
/// The Responses API accepts `[{"type": "input_text", ...}]` for tool output,
/// but several OpenAI-compatible gateways (LiteLLM-based ones included) fail
/// to convert it and return HTTP 400.
pub(crate) fn flatten_tool_output_to_text(item: &mut ResponseItem) {
    let output = match item {
        ResponseItem::CustomToolCallOutput { output, .. }
        | ResponseItem::FunctionCallOutput { output, .. } => output,
        _ => return,
    };
    let FunctionCallOutputBody::ContentItems(items) = &output.body else {
        return;
    };
    let text = items
        .iter()
        .filter_map(|item| match item {
            FunctionCallOutputContentItem::InputText { text } => Some(text.clone()),
            FunctionCallOutputContentItem::InputImage { .. }
            | FunctionCallOutputContentItem::InputAudio { .. }
            | FunctionCallOutputContentItem::EncryptedContent { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    output.body = FunctionCallOutputBody::Text(text);
}

/// Rewrites `developer`-role messages (and `additional_tools` items) to the
/// `system` role, which third-party backends that reject `developer` accept.
pub(crate) fn rewrite_developer_role_to_system(item: &mut ResponseItem) {
    fn downgrade(role: &mut String) {
        if role == "developer" {
            *role = "system".to_string();
        }
    }
    match item {
        ResponseItem::Message { role, .. } | ResponseItem::AdditionalTools { role, .. } => {
            downgrade(role);
        }
        _ => {}
    }
}

/// Rewrites a multi-agent `agent_message` input item into a plain assistant
/// message.
///
/// The Responses API accepts a custom `agent_message` item type; OpenAI-compatible
/// gateways (LiteLLM included) reject it with a validation error that names the
/// field (`body.input.N: Input should be 'message'`). Rewriting keeps the text
/// the model contributed while using only item types every gateway understands.
///
/// Returns `false` when the item must be dropped: an agent message whose content
/// is encrypted-only or blank cannot be represented as a plain message, and
/// forwarding it unchanged is a hard HTTP 400.
pub(crate) fn rewrite_agent_message_as_assistant_text(item: &mut ResponseItem) -> bool {
    let ResponseItem::AgentMessage { content, .. } = item else {
        return true;
    };
    let Some(text) = plaintext_agent_message_content(content) else {
        return false;
    };
    *item = ResponseItem::Message {
        id: None,
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText {
            text,
            annotations: None,
            logprobs: None,
        }],
        phase: None,
        status: None,
        encrypted_content: None,
        internal_chat_message_metadata_passthrough: None,
    };
    true
}
