use super::DELIVERY_SELF_CHECK_INSTRUCTIONS;
use super::compose_developer_instructions_with_self_check;

#[test]
fn disabled_returns_the_original_instructions() {
    assert_eq!(
        compose_developer_instructions_with_self_check(Some("keep me"), false).as_deref(),
        Some("keep me")
    );
    assert_eq!(
        compose_developer_instructions_with_self_check(None, false),
        None
    );
}

#[test]
fn enabled_appends_exactly_one_paragraph() {
    let composed = compose_developer_instructions_with_self_check(Some("base text"), true)
        .expect("enabled composition always yields instructions");
    assert_eq!(
        composed,
        format!("base text\n\n{DELIVERY_SELF_CHECK_INSTRUCTIONS}")
    );
    assert_eq!(
        composed.matches(DELIVERY_SELF_CHECK_INSTRUCTIONS).count(),
        1
    );
}

#[test]
fn enabled_without_existing_instructions_uses_the_paragraph_alone() {
    assert_eq!(
        compose_developer_instructions_with_self_check(None, true).as_deref(),
        Some(DELIVERY_SELF_CHECK_INSTRUCTIONS)
    );
    assert_eq!(
        compose_developer_instructions_with_self_check(Some(""), true).as_deref(),
        Some(DELIVERY_SELF_CHECK_INSTRUCTIONS)
    );
}

#[test]
fn instruction_text_is_generic() {
    let lowered = DELIVERY_SELF_CHECK_INSTRUCTIONS.to_ascii_lowercase();
    for forbidden in [
        "tbs",
        "benchmark",
        "p3a",
        "p3b",
        "eng9",
        "meep",
        "ode-law",
        "at11",
        "grader",
    ] {
        assert!(
            !lowered.contains(forbidden),
            "instruction text must stay generic; found `{forbidden}`"
        );
    }
    assert!(DELIVERY_SELF_CHECK_INSTRUCTIONS.is_ascii());
}

#[test]
fn instruction_text_names_the_receipt_field_once() {
    assert_eq!(
        DELIVERY_SELF_CHECK_INSTRUCTIONS
            .matches("validation_note:")
            .count(),
        1
    );
}
