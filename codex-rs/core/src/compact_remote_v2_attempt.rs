use std::sync::Arc;

use super::RemoteCompactionV2Output;
use super::run_remote_compaction_request_v2;
use crate::Prompt;
use crate::client::ModelClientSession;
use crate::compact::CompactionAnalyticsDetails;
use crate::compact_remote::REMOTE_COMPACTION_CONTEXT_RETRY_TARGET_PERCENTS;
use crate::compact_remote::trim_function_call_history_to_fit_context_window;
use crate::compact_remote::trim_history_for_remote_compaction_context_retry;
use crate::responses_metadata::CodexResponsesRequestKind;
use crate::responses_metadata::CompactionTurnMetadata;
use crate::session::session::Session;
use crate::session::step_context::StepContext;
use codex_protocol::error::CodexErrorDetails;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::RawResponseCompletedEvent;
use codex_protocol::protocol::TokenUsage;
use codex_rollout_trace::CompactionTraceContext;
use tracing::info;
use tracing::warn;

pub(super) struct RemoteCompactV2Attempt {
    pub(super) trace_input_history: Option<Vec<ResponseItem>>,
    pub(super) prompt_input: Vec<ResponseItem>,
    pub(super) compaction_output: ResponseItem,
    pub(super) token_usage: Option<TokenUsage>,
    /// Keeps a session created for standalone compaction alive through lifecycle completion.
    pub(super) owned_client_session: Option<ModelClientSession>,
}

pub(super) async fn run_remote_compact_v2_attempt(
    sess: &Arc<Session>,
    step_context: &Arc<StepContext>,
    mut client_session: Option<&mut ModelClientSession>,
    compaction_trace: &CompactionTraceContext,
    compaction_metadata: CompactionTurnMetadata,
    analytics_details: &mut CompactionAnalyticsDetails,
) -> CodexResult<RemoteCompactV2Attempt> {
    let turn_context = &step_context.turn;
    let mut history = sess.clone_history().await;
    let base_instructions = sess.get_base_instructions().await;
    let (rewritten_outputs, estimated_deleted_tokens) =
        trim_function_call_history_to_fit_context_window(
            &mut history,
            turn_context.as_ref(),
            &base_instructions,
        );
    if rewritten_outputs > 0 {
        info!(
            turn_id = %turn_context.sub_id,
            rewritten_outputs,
            "rewrote history outputs before remote compaction v2"
        );
    }
    if estimated_deleted_tokens > 0 {
        let max_local_deleted_tokens = sess
            .estimated_tokens_after_last_model_generated_item()
            .await;
        analytics_details.active_context_tokens_before = analytics_details
            .active_context_tokens_before
            .map(|active_context_tokens_before| {
                active_context_tokens_before
                    .saturating_sub(estimated_deleted_tokens.min(max_local_deleted_tokens))
            });
    }

    let window_id = sess.current_window_id().await;
    let responses_metadata = turn_context.turn_metadata_state.to_responses_metadata(
        sess.installation_id.clone(),
        window_id,
        CodexResponsesRequestKind::Compaction(compaction_metadata),
    );
    let mut owned_client_session = None;
    let mut context_retry_index = 0usize;
    loop {
        let trace_input_history = compaction_trace
            .is_enabled()
            .then(|| history.raw_items().to_vec());
        let mut input = history
            .clone()
            .for_prompt(&turn_context.model_info.input_modalities);
        let tool_router = &step_context.tool_router;
        input.push(ResponseItem::CompactionTrigger {});
        let prompt = Prompt {
            input,
            tools: tool_router.model_visible_specs(),
            parallel_tool_calls: turn_context.model_info.supports_parallel_tool_calls,
            base_instructions: base_instructions.clone(),
            output_schema: None,
            output_schema_strict: true,
        };
        let trace_attempt = compaction_trace.start_attempt(&serde_json::json!({
            "model": turn_context.model_info.slug.as_str(),
            "instructions": prompt.base_instructions.text.as_str(),
            "input": &prompt.input,
            "parallel_tool_calls": prompt.parallel_tool_calls,
        }));
        let active_client_session = match client_session.as_deref_mut() {
            Some(client_session) => client_session,
            None => {
                owned_client_session.get_or_insert_with(|| sess.services.model_client.new_session())
            }
        };
        let compaction_output_result = run_remote_compaction_request_v2(
            sess,
            turn_context.as_ref(),
            active_client_session,
            &prompt,
            &responses_metadata,
        )
        .await;
        trace_attempt.record_result(
            compaction_output_result
                .as_ref()
                .map(|output| std::slice::from_ref(&output.compaction_output)),
        );

        let RemoteCompactionV2Output {
            compaction_output,
            response_id,
            token_usage,
        } = match compaction_output_result {
            Ok(output) => output,
            Err(err)
                if matches!(err.details(), CodexErrorDetails::ContextWindowExceeded)
                    && context_retry_index
                        < REMOTE_COMPACTION_CONTEXT_RETRY_TARGET_PERCENTS.len() =>
            {
                let target_percent =
                    REMOTE_COMPACTION_CONTEXT_RETRY_TARGET_PERCENTS[context_retry_index];
                let (removed_items, estimated_deleted_tokens) =
                    trim_history_for_remote_compaction_context_retry(
                        &mut history,
                        turn_context.as_ref(),
                        &base_instructions,
                        target_percent,
                    );
                if removed_items == 0 {
                    return Err(err);
                }
                analytics_details.active_context_tokens_before = analytics_details
                    .active_context_tokens_before
                    .map(|active_context_tokens_before| {
                        active_context_tokens_before.saturating_sub(estimated_deleted_tokens)
                    });
                context_retry_index += 1;
                warn!(
                    turn_id = %turn_context.sub_id,
                    context_retry_index,
                    removed_items,
                    estimated_deleted_tokens,
                    target_percent,
                    "remote compaction v2 exceeded provider context; retrying with recent history"
                );
                continue;
            }
            Err(err) => return Err(err),
        };
        // TODO: Emit this before compaction output validation so malformed completed
        // responses still surface their raw upstream usage.
        sess.send_event(
            turn_context,
            EventMsg::RawResponseCompleted(RawResponseCompletedEvent {
                response_id,
                token_usage: token_usage.clone(),
            }),
        )
        .await;
        let mut prompt_input = prompt.input;
        prompt_input.pop();
        return Ok(RemoteCompactV2Attempt {
            trace_input_history,
            prompt_input,
            compaction_output,
            token_usage,
            owned_client_session,
        });
    }
}
