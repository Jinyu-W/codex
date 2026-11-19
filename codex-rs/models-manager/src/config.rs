use codex_protocol::config_types::Personality;
use codex_protocol::openai_models::ModelsResponse;

#[derive(Debug, Clone, Default)]
pub struct ModelsManagerConfig {
    pub model_context_window: Option<i64>,
    pub model_auto_compact_token_limit: Option<i64>,
    pub tool_output_token_limit: Option<usize>,
    pub base_instructions: Option<String>,
    pub personality: Option<Personality>,
    pub model_catalog: Option<ModelsResponse>,
    /// Give third-party provider models the OpenAI-equivalent tool surface.
    pub provider_parity: bool,
    /// Send free-form custom tools as `{"type": "text"}` instead of lark grammar.
    pub freeform_tools_as_text: bool,
}
