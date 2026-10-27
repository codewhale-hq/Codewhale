//! Plan hand-off: ask how to continue a successfully completed Plan response.
//!
//! The exact completed text is frozen for one session and turn. A prose plan
//! seeds one bounded pending item; it is not parsed into English-shaped steps.
//! Checkpoint admission uses the incumbent queued boundary, not a disk-write
//! acknowledgement. Failed preparation removes only its unchanged seed from
//! the current To-do projection; the graph retains unprojected history. That
//! repair cannot recall a checkpoint already accepted by the persistence actor.
//! If cleanup refuses because the owned item changed or Work validation fails,
//! retain its ownership and refuse a new dispatch rather than erase other work.
//!
//! The question uses the modal `request_user_input` already draws. It is
//! host-owned: no tool call is waiting in the engine, so the answer is applied
//! by the TUI (permission, mode, follow-up message) instead of being returned
//! as a tool result.

use codewhale_config::AppMode;
use codewhale_execpolicy::ApprovalMode;
use codewhale_localization::{Locale, MessageId, tr};

use crate::core::events::TurnOutcomeStatus;
use crate::tools::user_input::{
    UserInputOption, UserInputQuestion, UserInputRequest, UserInputResponse,
};

/// Request id of the hand-off question on the view stack.
pub(crate) const REQUEST_ID: &str = "codewhale:plan-handoff";
const QUESTION_ID: &str = "plan_handoff";

/// What the person chose to do with a finished plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PlanHandoffChoice {
    /// Leave Plan for Work with this permission and start on the plan.
    Work(ApprovalMode),
    /// Stay in Plan and send this feedback as the next message.
    Revise(String),
    /// Stay in Plan; nothing is sent.
    KeepPlanning,
}

/// One completed Plan response. The question id prevents a delayed answer
/// from approving a newer plan, even in the same session.
#[derive(Debug, Clone)]
pub(crate) struct PendingPlanHandoff {
    pub request_id: String,
    pub session_id: String,
    pub turn_id: String,
    pub history_start: usize,
    pub transcript_epoch: u64,
    pub text: String,
    pub has_current_checklist: bool,
    /// Retained only if failed-preparation cleanup cannot safely remove it.
    pub seeded_todo_id: Option<u32>,
}

/// Only typed, successful, nonempty output from the current Plan turn qualifies.
pub(crate) fn plan_ready(mode: AppMode, status: TurnOutcomeStatus, output: Option<&str>) -> bool {
    mode == AppMode::Plan
        && status == TurnOutcomeStatus::Completed
        && output.is_some_and(|text| !text.trim().is_empty())
}

/// The hand-off question, in the person's language.
pub(crate) fn request(locale: Locale) -> UserInputRequest {
    let option = |label: MessageId, description: MessageId| UserInputOption {
        label: tr(locale, label).into_owned(),
        description: tr(locale, description).into_owned(),
    };
    UserInputRequest {
        require_typed_answers: false,
        questions: vec![UserInputQuestion {
            detail: None,
            intent: None,
            header: tr(locale, MessageId::PlanHandoffHeader).into_owned(),
            id: QUESTION_ID.to_string(),
            question: tr(locale, MessageId::PlanHandoffQuestion).into_owned(),
            options: vec![
                option(
                    MessageId::PlanHandoffWorkAsk,
                    MessageId::PlanHandoffWorkAskDetail,
                ),
                option(
                    MessageId::PlanHandoffWorkAuto,
                    MessageId::PlanHandoffWorkAutoDetail,
                ),
                option(
                    MessageId::PlanHandoffKeepPlanning,
                    MessageId::PlanHandoffKeepPlanningDetail,
                ),
            ],
            allow_free_text: true,
            multi_select: false,
        }],
    }
}

/// Read the answer to [`request`]. A typed response is feedback on the plan;
/// anything unrecognized or empty keeps planning, the choice that changes
/// nothing.
pub(crate) fn choice(locale: Locale, response: &UserInputResponse) -> PlanHandoffChoice {
    let Some(answer) = response.answers.first() else {
        return PlanHandoffChoice::KeepPlanning;
    };
    let picked = |id: MessageId| answer.label == tr(locale, id);
    if picked(MessageId::PlanHandoffWorkAsk) {
        PlanHandoffChoice::Work(ApprovalMode::Suggest)
    } else if picked(MessageId::PlanHandoffWorkAuto) {
        PlanHandoffChoice::Work(ApprovalMode::Auto)
    } else if picked(MessageId::PlanHandoffKeepPlanning) {
        PlanHandoffChoice::KeepPlanning
    } else {
        let feedback = answer.value.trim();
        if feedback.is_empty() {
            PlanHandoffChoice::KeepPlanning
        } else {
            PlanHandoffChoice::Revise(feedback.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::user_input::UserInputAnswer;

    fn answer(label: &str, value: &str) -> UserInputResponse {
        UserInputResponse {
            answers: vec![UserInputAnswer {
                id: QUESTION_ID.to_string(),
                label: label.to_string(),
                value: value.to_string(),
                kind: None,
            }],
        }
    }

    #[test]
    fn only_successful_nonempty_plan_output_is_ready() {
        let prose = Some("先检查数据。\n\nThen build the report.");
        assert!(plan_ready(
            AppMode::Plan,
            TurnOutcomeStatus::Completed,
            prose
        ));
        for mode in [AppMode::Agent, AppMode::Operate] {
            assert!(!plan_ready(mode, TurnOutcomeStatus::Completed, prose));
        }
        for status in [TurnOutcomeStatus::Interrupted, TurnOutcomeStatus::Failed] {
            assert!(!plan_ready(AppMode::Plan, status, prose));
        }
        for output in [None, Some(""), Some(" \n ")] {
            assert!(!plan_ready(
                AppMode::Plan,
                TurnOutcomeStatus::Completed,
                output
            ));
        }
    }

    #[test]
    fn every_locale_asks_a_valid_question_and_reads_its_own_answers() {
        for locale in Locale::shipped_complete() {
            let asked = request(*locale);
            asked
                .validate()
                .unwrap_or_else(|error| panic!("{}: {error:?}", locale.tag()));
            let labels: Vec<&str> = asked.questions[0]
                .options
                .iter()
                .map(|option| option.label.as_str())
                .collect();
            let expected = [
                PlanHandoffChoice::Work(ApprovalMode::Suggest),
                PlanHandoffChoice::Work(ApprovalMode::Auto),
                PlanHandoffChoice::KeepPlanning,
            ];
            assert_eq!(labels.len(), expected.len(), "{}", locale.tag());
            for (label, expected) in labels.iter().zip(expected) {
                // The modal answers an option with its label as the value.
                assert_eq!(
                    choice(*locale, &answer(label, label)),
                    expected,
                    "{} {label}",
                    locale.tag()
                );
                // "Other" is the modal's own free-text row.
                assert_ne!(*label, "Other", "{}", locale.tag());
            }
        }
    }

    #[test]
    fn a_typed_response_is_feedback_and_an_empty_one_keeps_planning() {
        assert_eq!(
            choice(Locale::En, &answer("Other", "  cover the tests too ")),
            PlanHandoffChoice::Revise("cover the tests too".to_string())
        );
        assert_eq!(
            choice(Locale::En, &answer("Other", "   ")),
            PlanHandoffChoice::KeepPlanning
        );
        assert_eq!(
            choice(Locale::En, &UserInputResponse { answers: vec![] }),
            PlanHandoffChoice::KeepPlanning
        );
    }
}
