//! Tool and types for requesting user input via the TUI.

use super::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec,
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashSet;

/// Default ceiling on `request_user_input.questions` (#5949). Raised from the
/// former hard-coded 3 so research/planning turns that need four or more
/// clarifications are not rejected outright.
pub const DEFAULT_MAX_QUESTIONS: usize = 6;
/// Default ceiling on options per question.
pub const DEFAULT_MAX_OPTIONS: usize = 4;
/// Inclusive bounds a configured `user_input_max_questions` is clamped into.
pub const MIN_CONFIGURABLE_QUESTIONS: usize = 1;
pub const MAX_CONFIGURABLE_QUESTIONS: usize = 10;
/// Inclusive bounds a configured `user_input_max_options` is clamped into.
/// The floor is 2 because a one-option question is not a choice.
pub const MIN_CONFIGURABLE_OPTIONS: usize = 2;
pub const MAX_CONFIGURABLE_OPTIONS: usize = 10;

/// Config key naming used in both the clamp WARN and the rejection message, so
/// a model that hits the ceiling is told exactly where to raise it.
pub const MAX_QUESTIONS_KEY: &str = "[tools] user_input_max_questions";
pub const MAX_OPTIONS_KEY: &str = "[tools] user_input_max_options";

/// Effective `request_user_input` payload ceilings for one session.
///
/// Resolved once from `[tools]` in config.toml (see
/// [`UserInputLimits::from_config`]) and carried to the three
/// places that must agree: the validator, the tool's JSON schema, and its
/// model-visible description.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserInputLimits {
    pub max_questions: usize,
    pub max_options: usize,
}

impl Default for UserInputLimits {
    fn default() -> Self {
        Self {
            max_questions: DEFAULT_MAX_QUESTIONS,
            max_options: DEFAULT_MAX_OPTIONS,
        }
    }
}

impl UserInputLimits {
    /// Effective `request_user_input` payload ceilings for this session
    /// (#5949). Resolved once per engine build; out-of-range `[tools]` values
    /// clamp with a single warning naming the key and the value used.
    #[must_use]
    pub fn from_config(config: &codewhale_runtime::config::Config) -> Self {
        let tools = config.tools.as_ref();
        Self::from_config_values(
            tools.and_then(|t| t.user_input_max_questions),
            tools.and_then(|t| t.user_input_max_options),
        )
    }

    /// Resolve raw `[tools]` values, clamping each into its supported range.
    /// An out-of-range value is honoured as far as it can be rather than
    /// failing the load, and says so once with the key and the value used.
    #[must_use]
    pub fn from_config_values(max_questions: Option<u32>, max_options: Option<u32>) -> Self {
        Self {
            max_questions: clamp_with_warn(
                max_questions,
                DEFAULT_MAX_QUESTIONS,
                MIN_CONFIGURABLE_QUESTIONS,
                MAX_CONFIGURABLE_QUESTIONS,
                MAX_QUESTIONS_KEY,
            ),
            max_options: clamp_with_warn(
                max_options,
                DEFAULT_MAX_OPTIONS,
                MIN_CONFIGURABLE_OPTIONS,
                MAX_CONFIGURABLE_OPTIONS,
                MAX_OPTIONS_KEY,
            ),
        }
    }
}

fn clamp_with_warn(raw: Option<u32>, default: usize, min: usize, max: usize, key: &str) -> usize {
    let Some(raw) = raw else {
        return default;
    };
    let raw = raw as usize;
    let clamped = raw.clamp(min, max);
    if clamped != raw {
        tracing::warn!(
            "`{key}` = {raw} is outside the supported range {min}..={max}; using {clamped}"
        );
    }
    clamped
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct UserInputOption {
    pub label: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum UserInputIntent {
    PlanReview {
        approve: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        call_id: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct UserInputQuestion {
    #[serde(default)]
    pub header: String,
    pub id: String,
    pub question: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent: Option<UserInputIntent>,
    #[serde(default)]
    pub options: Vec<UserInputOption>,
    /// When `true`, the modal offers a free-text "Other" response in addition
    /// to the fixed options. Defaults to `false` for backwards compatibility
    /// (older payloads omitting the field get the previous behavior).
    #[serde(default)]
    pub allow_free_text: bool,
    /// When `true`, the user may select more than one option before confirming.
    #[serde(default)]
    pub multi_select: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct UserInputRequest {
    #[serde(default)]
    pub require_typed_answers: bool,
    pub questions: Vec<UserInputQuestion>,
}

impl UserInputRequest {
    /// Parse and validate against the built-in default limits. Call sites that
    /// hold a session's resolved config use [`Self::from_value_with_limits`].
    #[cfg(test)]
    pub fn from_value(value: &Value) -> Result<Self, ToolError> {
        Self::from_value_with_limits(value, UserInputLimits::default())
    }

    pub fn from_value_with_limits(
        value: &Value,
        limits: UserInputLimits,
    ) -> Result<Self, ToolError> {
        let mut request: UserInputRequest = serde_json::from_value(value.clone()).map_err(|e| {
            ToolError::invalid_input(format!("Invalid request_user_input payload: {e}"))
        })?;
        request.require_typed_answers = false;
        request.validate_with_limits(limits)?;
        Ok(request)
    }

    #[cfg(test)]
    pub fn validate(&self) -> Result<(), ToolError> {
        self.validate_with_limits(UserInputLimits::default())
    }

    pub fn validate_with_limits(&self, limits: UserInputLimits) -> Result<(), ToolError> {
        self.validate_transport()?;
        if self.questions.is_empty() {
            return Err(ToolError::invalid_input(
                "request_user_input.questions must be non-empty",
            ));
        }
        if self.questions.len() > limits.max_questions {
            // Name the ceiling *and* where to raise it: a rejection the model
            // cannot act on just burns another turn (#5949).
            return Err(ToolError::invalid_input(format!(
                "request_user_input.questions must contain 1 to {max} items (got {got}); \
                 raise the ceiling with `{MAX_QUESTIONS_KEY}` in config.toml \
                 (supported range {MIN_CONFIGURABLE_QUESTIONS}..={MAX_CONFIGURABLE_QUESTIONS})",
                max = limits.max_questions,
                got = self.questions.len(),
            )));
        }
        for q in &self.questions {
            if q.options.len() < MIN_CONFIGURABLE_OPTIONS || q.options.len() > limits.max_options {
                return Err(ToolError::invalid_input(format!(
                    "request_user_input.questions.options must contain \
                     {MIN_CONFIGURABLE_OPTIONS} to {max} items (got {got}); \
                     raise the ceiling with `{MAX_OPTIONS_KEY}` in config.toml \
                     (supported range {MIN_CONFIGURABLE_OPTIONS}..={MAX_CONFIGURABLE_OPTIONS})",
                    max = limits.max_options,
                    got = q.options.len(),
                )));
            }
        }
        Ok(())
    }

    pub fn validate_transport(&self) -> Result<(), ToolError> {
        if self.questions.is_empty() || self.questions.len() > MAX_CONFIGURABLE_QUESTIONS {
            return Err(ToolError::invalid_input(
                "User input must contain 1 to 10 questions",
            ));
        }
        if serde_json::to_vec(self)
            .map_err(|_| ToolError::invalid_input("Invalid user input request"))?
            .len()
            > 1024 * 1024
        {
            return Err(ToolError::invalid_input("User input request exceeds 1 MiB"));
        }
        let mut ids = HashSet::new();
        for question in &self.questions {
            if question.id.trim().is_empty()
                || question.question.trim().is_empty()
                || !ids.insert(&question.id)
            {
                return Err(ToolError::invalid_input(
                    "User input questions require unique nonempty ids and nonempty text",
                ));
            }
            if question.options.len() > 1000
                || (question.options.is_empty() && !question.allow_free_text)
            {
                return Err(ToolError::invalid_input(
                    "User input supports at most 1000 choices per question; zero choices requires free text",
                ));
            }
            let mut labels = HashSet::new();
            for option in &question.options {
                if option.label.trim().is_empty() || !labels.insert(&option.label) {
                    return Err(ToolError::invalid_input(
                        "User input choices require unique nonempty labels",
                    ));
                }
            }
            if let Some(UserInputIntent::PlanReview { approve, .. }) = &question.intent {
                if !labels.contains(approve)
                    || question
                        .detail
                        .as_ref()
                        .is_none_or(|detail| detail.trim().is_empty())
                {
                    return Err(ToolError::invalid_input(
                        "Plan review requires detail and an offered approve choice",
                    ));
                }
            }
        }
        Ok(())
    }

    pub fn validate_response(&self, response: &UserInputResponse) -> Result<(), ToolError> {
        if response.answers.len() > 10020
            || serde_json::to_vec(response)
                .map_err(|_| ToolError::invalid_input("Invalid user input response"))?
                .len()
                > 1024 * 1024
        {
            return Err(ToolError::invalid_input(
                "User input response exceeds its transport limits",
            ));
        }
        if response.answers.iter().any(|answer| {
            !self
                .questions
                .iter()
                .any(|question| question.id == answer.id)
        }) {
            return Err(ToolError::invalid_input("Answer names an unknown question"));
        }
        if !self.require_typed_answers
            && response.answers.iter().all(|answer| answer.kind.is_none())
        {
            return Ok(());
        }
        if response.answers.iter().any(|answer| answer.kind.is_none()) {
            return Err(ToolError::invalid_input(
                "This question requires typed answers; use a client that supports choice, custom and skipped answers",
            ));
        }
        for question in &self.questions {
            let mut choices = HashSet::new();
            let mut custom = false;
            let mut skipped = false;
            let mut count = 0;
            for answer in response
                .answers
                .iter()
                .filter(|answer| answer.id == question.id)
            {
                count += 1;
                match answer.kind {
                    Some(UserInputAnswerKind::Choice) => {
                        if answer.label != answer.value
                            || !question
                                .options
                                .iter()
                                .any(|option| option.label == answer.value)
                            || !choices.insert(&answer.value)
                        {
                            return Err(ToolError::invalid_input(
                                "Answer must select each offered label at most once",
                            ));
                        }
                    }
                    Some(UserInputAnswerKind::Custom) => {
                        if (self.require_typed_answers && !question.allow_free_text)
                            || custom
                            || !answer.label.is_empty()
                            || answer.value.trim().is_empty()
                        {
                            return Err(ToolError::invalid_input("Invalid custom answer"));
                        }
                        custom = true;
                    }
                    Some(UserInputAnswerKind::Skipped) => {
                        if skipped || !answer.label.is_empty() || !answer.value.is_empty() {
                            return Err(ToolError::invalid_input("Invalid skipped answer"));
                        }
                        skipped = true;
                    }
                    None => unreachable!(),
                }
            }
            if count == 0
                || (skipped && count != 1)
                || (!question.multi_select && choices.len() + usize::from(custom) > 1)
            {
                return Err(ToolError::invalid_input(
                    "Answer must explicitly answer or skip each question and respect single selection",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum UserInputAnswerKind {
    Choice,
    Custom,
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct UserInputAnswer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<UserInputAnswerKind>,
    pub id: String,
    pub label: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct UserInputResponse {
    pub answers: Vec<UserInputAnswer>,
}

pub struct RequestUserInputTool {
    limits: UserInputLimits,
    /// Rendered once at construction: `description` hands out a borrow, and the
    /// effective ceiling only changes when the registry is rebuilt.
    description: String,
}

impl Default for RequestUserInputTool {
    fn default() -> Self {
        Self::new(UserInputLimits::default())
    }
}

impl RequestUserInputTool {
    #[must_use]
    pub fn new(limits: UserInputLimits) -> Self {
        Self {
            description: format!(
                "Ask the user 1-{} short questions with selectable options and return their \
selections. It is for decisions that are the user's to make, where a guess would be costly \
or wrong: ambiguous scope, an irreversible or expensive choice, a missing preference, or a \
fork the user should own. It returns the user's choices, not facts about the workspace. The \
call blocks until the user answers.",
                limits.max_questions
            ),
            limits,
        }
    }
}

#[async_trait]
impl ToolSpec for RequestUserInputTool {
    fn name(&self) -> &'static str {
        "request_user_input"
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "questions": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "header": { "type": "string" },
                            "id": { "type": "string" },
                            "question": { "type": "string" },
                            "options": {
                                "type": "array",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "label": { "type": "string" },
                                        "description": { "type": "string" }
                                    },
                                    "required": ["label"]
                                },
                                "minItems": MIN_CONFIGURABLE_OPTIONS,
                                "maxItems": self.limits.max_options
                            },
                            "allow_free_text": {
                                "type": "boolean",
                                "description": "When true, also offer a free-text 'Other' response. Defaults to false.",
                                "default": false
                            },
                            "multi_select": {
                                "type": "boolean",
                                "description": "When true, allow selecting more than one option. Defaults to false.",
                                "default": false
                            }
                        },
                        "required": ["id", "question", "options"]
                    },
                    "minItems": 1,
                    "maxItems": self.limits.max_questions
                }
            },
            "required": ["questions"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ReadOnly]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Auto
    }

    async fn execute(
        &self,
        _input: Value,
        _context: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        Err(ToolError::execution_failed(
            "request_user_input must be handled by the engine",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_request_shape() {
        let request = UserInputRequest {
            require_typed_answers: false,
            questions: vec![UserInputQuestion {
                header: "Pick".to_string(),
                id: "choice".to_string(),
                question: "Which option?".to_string(),
                detail: None,
                intent: None,
                options: vec![
                    UserInputOption {
                        label: "A".to_string(),
                        description: "Option A".to_string(),
                    },
                    UserInputOption {
                        label: "B".to_string(),
                        description: "Option B".to_string(),
                    },
                ],
                allow_free_text: false,
                multi_select: false,
            }],
        };
        assert!(request.validate().is_ok());
        for count in [0, 1, 100, 1000] {
            let menu: UserInputRequest = serde_json::from_value(json!({
                "require_typed_answers":true,
                "questions":[{"id":"menu", "question":"Choose", "allow_free_text":true,
                    "options":(0..count).map(|i| json!({"label":format!("Choice {i}")})).collect::<Vec<_>>() }]
            })).unwrap();
            assert!(menu.validate_transport().is_ok());
            assert!(menu.validate().is_err());
            assert!(menu.questions[0].header.is_empty());
            assert!(
                menu.questions[0]
                    .options
                    .iter()
                    .all(|option| option.description.is_empty())
            );
        }
        let mut menu: UserInputRequest = serde_json::from_value(json!({
            "require_typed_answers":true,
            "questions":[{"id":"menu", "question":"Choose", "allow_free_text":true, "multi_select":true,
                "options":[{"label":"Other"},{"label":"left, right"}]}]
        })).unwrap();
        let mut response: UserInputResponse = serde_json::from_value(json!({"answers":[
            {"id":"menu","kind":"choice","label":"Other","value":"Other"},
            {"id":"menu","kind":"choice","label":"left, right","value":"left, right"},
            {"id":"menu","kind":"custom","label":"","value":"custom text"}
        ]}))
        .unwrap();
        assert!(menu.validate_response(&response).is_ok());
        let exact = serde_json::to_value(&response).unwrap();
        assert_eq!(exact["answers"][1]["value"], "left, right");
        menu.questions[0].multi_select = false;
        assert!(menu.validate_response(&response).is_err());
        menu.questions[0].multi_select = true;
        response.answers.push(response.answers[0].clone());
        assert!(menu.validate_response(&response).is_err());
        response.answers.pop();
        response.answers[0].kind = None;
        assert!(menu.validate_response(&response).is_err());
        for answers in [
            json!([]),
            json!([{"id":"unknown","kind":"skipped","label":"","value":""}]),
            json!([{"id":"menu","kind":"choice","label":"unknown","value":"unknown"}]),
            json!([{"id":"menu","kind":"choice","label":"Other","value":"wrong"}]),
            json!([{"id":"menu","kind":"custom","label":"Other","value":"text"}]),
            json!([{"id":"menu","kind":"custom","label":"","value":" "}]),
            json!([{"id":"menu","kind":"skipped","label":"","value":""},{"id":"menu","kind":"custom","label":"","value":"text"}]),
        ] {
            let response = serde_json::from_value(json!({"answers":answers})).unwrap();
            assert!(menu.validate_response(&response).is_err());
        }
        let skipped = serde_json::from_value(
            json!({"answers":[{"id":"menu","kind":"skipped","label":"","value":""}]}),
        )
        .unwrap();
        assert!(menu.validate_response(&skipped).is_ok());
        let custom = serde_json::from_value(
            json!({"answers":[{"id":"menu","kind":"custom","label":"","value":"legacy custom"}]}),
        )
        .unwrap();
        menu.questions[0].allow_free_text = false;
        assert!(menu.validate_response(&custom).is_err());
        menu.require_typed_answers = false;
        assert!(menu.validate_response(&custom).is_ok());
        let mixed = serde_json::from_value(json!({"answers":[{"id":"menu","kind":"custom","label":"","value":"legacy custom"},{"id":"menu","label":"Other","value":"Other"}]})).unwrap();
        assert!(menu.validate_response(&mixed).is_err());
        menu.require_typed_answers = true;
        menu.questions[0].allow_free_text = true;
        menu.questions[0].intent = Some(UserInputIntent::PlanReview {
            approve: "Other".into(),
            call_id: Some("display-only".into()),
        });
        assert!(menu.validate_transport().is_err());
        menu.questions[0].detail = Some("Plan detail".into());
        assert!(menu.validate_transport().is_ok());
        menu.questions[0].options[0].label = "Different".into();
        assert!(menu.validate_transport().is_err());
        menu.questions[0].intent = None;
        menu.questions.push(menu.questions[0].clone());
        assert!(menu.validate_transport().is_err());
        menu.questions.pop();
        menu.questions[0].options = vec![
            UserInputOption {
                label: "duplicate".into(),
                description: String::new()
            };
            1001
        ];
        assert!(menu.validate_transport().is_err());
        menu.questions[0].options.truncate(2);
        assert!(menu.validate_transport().is_err());
        menu.questions[0].options.clear();
        menu.questions[0].allow_free_text = false;
        assert!(menu.validate_transport().is_err());
        menu.questions[0].allow_free_text = true;
        menu.questions[0].detail = Some("x".repeat(1024 * 1024));
        assert!(menu.validate_transport().is_err());
    }

    #[test]
    fn from_value_accepts_four_options_and_flags() {
        // Mirrors the json!-literal style used in tools/subagent/tests.rs and
        // exercises the schema-loosening from issue #3102: 4 options (was capped
        // at 3) plus the new allow_free_text / multi_select flags.
        let input = json!({
            "questions": [{
                "header": "Scope",
                "id": "scope",
                "question": "Which surfaces should this change affect?",
                "options": [
                    { "label": "TUI", "description": "Visible modal flow only" },
                    { "label": "Headless", "description": "Protocol event only" },
                    { "label": "All surfaces", "description": "TUI and headless" },
                    { "label": "CLI", "description": "Command-line surface" }
                ],
                "allow_free_text": true,
                "multi_select": true
            }]
        });
        let request = UserInputRequest::from_value(&input).expect("4 options + flags parse");
        assert_eq!(request.questions.len(), 1);
        assert_eq!(request.questions[0].options.len(), 4);
        assert!(request.questions[0].allow_free_text);
        assert!(request.questions[0].multi_select);
        let mut input = input;
        input["require_typed_answers"] = json!(true);
        assert!(
            !UserInputRequest::from_value(&input)
                .unwrap()
                .require_typed_answers
        );
    }

    #[test]
    fn from_value_defaults_flags_when_omitted() {
        // Backwards compatibility: a legacy payload omitting the new boolean
        // fields must still parse, defaulting both to false.
        let input = json!({
            "questions": [{
                "header": "Pick",
                "id": "choice",
                "question": "Which?",
                "options": [
                    { "label": "A", "description": "a" },
                    { "label": "B", "description": "b" }
                ]
            }]
        });
        let request = UserInputRequest::from_value(&input).expect("legacy payload parses");
        assert!(!request.questions[0].allow_free_text);
        assert!(!request.questions[0].multi_select);
    }

    #[test]
    fn rejects_five_options() {
        let input = json!({
            "questions": [{
                "header": "Pick",
                "id": "choice",
                "question": "Which?",
                "options": [
                    { "label": "A", "description": "a" },
                    { "label": "B", "description": "b" },
                    { "label": "C", "description": "c" },
                    { "label": "D", "description": "d" },
                    { "label": "E", "description": "e" }
                ]
            }]
        });
        let err = UserInputRequest::from_value(&input).expect_err("5 options must fail");
        assert!(err.to_string().contains("2 to 4 items"));
    }

    fn yes_no_question(header: &str, id: &str) -> UserInputQuestion {
        UserInputQuestion {
            header: header.to_string(),
            id: id.to_string(),
            question: "?".to_string(),
            detail: None,
            intent: None,
            options: vec![
                UserInputOption {
                    label: "A".to_string(),
                    description: "A".to_string(),
                },
                UserInputOption {
                    label: "B".to_string(),
                    description: "B".to_string(),
                },
            ],
            allow_free_text: false,
            multi_select: false,
        }
    }

    fn questions(count: usize) -> UserInputRequest {
        UserInputRequest {
            require_typed_answers: false,
            questions: (1..=count)
                .map(|i| yes_no_question(&format!("Q{i}"), &format!("q{i}")))
                .collect(),
        }
    }

    #[test]
    fn default_limits_are_six_questions_and_four_options() {
        let limits = UserInputLimits::default();
        assert_eq!(limits.max_questions, 6);
        assert_eq!(limits.max_options, 4);
        // An empty `[tools]` table resolves to the same ceilings.
        assert_eq!(UserInputLimits::from_config_values(None, None), limits);
        let tool = RequestUserInputTool::default();
        let description = tool.description();
        assert!(description.contains("Ask the user 1-6 short questions"));
        assert!(description.contains("blocks until the user answers"));
    }

    #[test]
    fn accepts_six_questions_by_default() {
        assert!(questions(6).validate().is_ok());
    }

    #[test]
    fn rejects_seven_questions_and_names_the_config_key() {
        let err = questions(7)
            .validate()
            .expect_err("7 questions exceeds the default ceiling of 6");
        let msg = err.to_string();
        assert!(msg.contains("1 to 6 items"), "{msg}");
        assert!(msg.contains("got 7"), "{msg}");
        assert!(msg.contains("[tools] user_input_max_questions"), "{msg}");
        assert!(msg.contains("config.toml"), "{msg}");
    }

    #[test]
    fn rejected_option_count_names_the_config_key() {
        let input = json!({
            "questions": [{
                "header": "Pick",
                "id": "choice",
                "question": "Which?",
                "options": [
                    { "label": "A", "description": "a" },
                    { "label": "B", "description": "b" },
                    { "label": "C", "description": "c" },
                    { "label": "D", "description": "d" },
                    { "label": "E", "description": "e" }
                ]
            }]
        });
        let msg = UserInputRequest::from_value(&input)
            .expect_err("5 options must fail")
            .to_string();
        assert!(msg.contains("[tools] user_input_max_options"), "{msg}");
    }

    #[test]
    fn configured_limits_widen_and_narrow_the_validator() {
        let wide = UserInputLimits::from_config_values(Some(9), Some(6));
        assert!(questions(9).validate_with_limits(wide).is_ok());

        let narrow = UserInputLimits::from_config_values(Some(2), None);
        let msg = questions(3)
            .validate_with_limits(narrow)
            .expect_err("3 questions exceeds a configured ceiling of 2")
            .to_string();
        assert!(msg.contains("1 to 2 items"), "{msg}");
    }

    #[test]
    fn out_of_range_config_values_clamp() {
        // Below the floor and far above the ceiling both clamp instead of
        // failing the config load.
        let low = UserInputLimits::from_config_values(Some(0), Some(0));
        assert_eq!(low.max_questions, MIN_CONFIGURABLE_QUESTIONS);
        assert_eq!(low.max_options, MIN_CONFIGURABLE_OPTIONS);

        let high = UserInputLimits::from_config_values(Some(50), Some(50));
        assert_eq!(high.max_questions, MAX_CONFIGURABLE_QUESTIONS);
        assert_eq!(high.max_options, MAX_CONFIGURABLE_OPTIONS);
    }

    #[test]
    fn schema_and_description_reflect_configured_limits() {
        let tool = RequestUserInputTool::new(UserInputLimits::from_config_values(Some(8), Some(5)));
        let description = tool.description();
        assert!(description.contains("Ask the user 1-8 short questions"));
        assert!(description.contains("blocks until the user answers"));
        let schema = tool.input_schema();
        let questions = &schema["properties"]["questions"];
        assert_eq!(questions["minItems"], json!(1));
        assert_eq!(questions["maxItems"], json!(8));
        let options = &questions["items"]["properties"]["options"];
        assert_eq!(options["minItems"], json!(2));
        assert_eq!(options["maxItems"], json!(5));

        // Defaults land on the documented 6 / 4 pair.
        let default_schema = RequestUserInputTool::default().input_schema();
        assert_eq!(
            default_schema["properties"]["questions"]["maxItems"],
            json!(6)
        );
        assert_eq!(
            default_schema["properties"]["questions"]["items"]["properties"]["options"]["maxItems"],
            json!(4)
        );
    }

    #[test]
    fn rejects_too_many_questions() {
        // Seven is one past the default ceiling; four now parses (#5949).
        assert!(questions(4).validate().is_ok());
        assert!(questions(7).validate().is_err());
    }
}
