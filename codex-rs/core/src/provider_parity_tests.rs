use super::*;

#[test]
fn freeform_downgrade_follows_mode() {
    let with = |enabled, mode| ProviderParityConfig {
        enabled,
        freeform_tool_format: mode,
        developer_role: DeveloperRoleMode::Auto,
    };
    assert!(with(true, FreeformToolFormatMode::Auto).downgrade_freeform_to_text());
    assert!(with(true, FreeformToolFormatMode::Text).downgrade_freeform_to_text());
    assert!(!with(true, FreeformToolFormatMode::Grammar).downgrade_freeform_to_text());
    assert!(!with(false, FreeformToolFormatMode::Auto).downgrade_freeform_to_text());
    assert!(with(false, FreeformToolFormatMode::Text).downgrade_freeform_to_text());
}

#[test]
fn tool_output_content_items_flatten_to_text() {
    use codex_protocol::models::FunctionCallOutputBody;
    use codex_protocol::models::FunctionCallOutputContentItem;
    use codex_protocol::models::FunctionCallOutputPayload;

    let mut item = ResponseItem::CustomToolCallOutput {
        id: None,
        call_id: "call_1".to_string(),
        name: None,
        internal_chat_message_metadata_passthrough: None,
        output: FunctionCallOutputPayload {
            body: FunctionCallOutputBody::ContentItems(vec![
                FunctionCallOutputContentItem::InputText {
                    text: "Script completed".to_string(),
                },
                FunctionCallOutputContentItem::InputText {
                    text: "parity ok".to_string(),
                },
            ]),
            success: None,
        },
    };

    flatten_tool_output_to_text(&mut item);

    let ResponseItem::CustomToolCallOutput { output, .. } = item else {
        panic!("expected custom tool call output");
    };
    assert_eq!(
        output.body,
        FunctionCallOutputBody::Text("Script completed\nparity ok".to_string())
    );
}

#[test]
fn developer_role_downgrade_follows_mode() {
    let with = |enabled, mode| ProviderParityConfig {
        enabled,
        freeform_tool_format: FreeformToolFormatMode::Auto,
        developer_role: mode,
    };
    assert!(with(true, DeveloperRoleMode::Auto).rewrite_developer_as_system());
    assert!(!with(false, DeveloperRoleMode::Auto).rewrite_developer_as_system());
    assert!(with(true, DeveloperRoleMode::System).rewrite_developer_as_system());
    assert!(!with(true, DeveloperRoleMode::Developer).rewrite_developer_as_system());
}

#[test]
fn developer_message_rewrites_to_system() {
    let mut item = ResponseItem::Message {
        encrypted_content: None,
        status: None,
        id: None,
        role: "developer".to_string(),
        content: Vec::new(),
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    };
    rewrite_developer_role_to_system(&mut item);
    match item {
        ResponseItem::Message { role, .. } => assert_eq!(role, "system"),
        other => panic!("unexpected item: {other:?}"),
    }
}

#[test]
fn agent_message_rewrites_to_assistant_message() {
    use codex_protocol::models::AgentMessageInputContent;

    let mut item = ResponseItem::AgentMessage {
        id: None,
        author: "agent-1".to_string(),
        recipient: "agent-2".to_string(),
        content: vec![AgentMessageInputContent::InputText {
            text: "handoff: the build is green".to_string(),
        }],
        internal_chat_message_metadata_passthrough: None,
    };
    assert!(rewrite_agent_message_as_assistant_text(&mut item));
    match item {
        ResponseItem::Message { role, content, .. } => {
            assert_eq!(role, "assistant");
            assert_eq!(
                content,
                vec![ContentItem::OutputText {
                    text: "handoff: the build is green".to_string(),
                    annotations: None,
                    logprobs: None,
                }]
            );
        }
        other => panic!("unexpected item: {other:?}"),
    }
}

#[test]
fn encrypted_agent_message_is_dropped() {
    use codex_protocol::models::AgentMessageInputContent;

    let mut item = ResponseItem::AgentMessage {
        id: None,
        author: "agent-1".to_string(),
        recipient: "agent-2".to_string(),
        content: vec![AgentMessageInputContent::EncryptedContent {
            encrypted_content: "gAAAA".to_string(),
        }],
        internal_chat_message_metadata_passthrough: None,
    };
    assert!(!rewrite_agent_message_as_assistant_text(&mut item));
}

