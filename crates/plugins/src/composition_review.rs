use std::sync::OnceLock;

use serde_json::Value;

pub type CompositionReviewFn = fn(&Value) -> Result<Value, String>;

static REVIEWER: OnceLock<CompositionReviewFn> = OnceLock::new();

pub fn install_composition_reviewer(reviewer: CompositionReviewFn) {
    let _ = REVIEWER.set(reviewer);
}

pub fn review(input: &Value) -> Result<Value, String> {
    let reviewer = REVIEWER
        .get()
        .ok_or_else(|| "composition reviewer is unavailable".to_string())?;
    reviewer(input)
}
