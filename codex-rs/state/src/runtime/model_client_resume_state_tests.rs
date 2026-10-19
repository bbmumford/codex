use super::*;
use crate::runtime::test_support::unique_temp_dir;
use codex_utils_absolute_path::test_support::PathExt;
use pretty_assertions::assert_eq;

async fn runtime() -> anyhow::Result<std::sync::Arc<StateRuntime>> {
    StateRuntime::init(
        crate::SqliteConfig::new_for_testing(unique_temp_dir().as_path().abs()),
        "test-provider".to_string(),
    )
    .await
}

#[tokio::test]
async fn resume_state_is_isolated_by_thread_and_account() -> anyhow::Result<()> {
    let runtime = runtime().await?;
    let thread_id = ThreadId::new();
    let state = ModelClientResumeState {
        account_id: "account-a".to_string(),
        turn_state: "turn-state-a".to_string(),
    };
    runtime
        .record_model_client_resume_state(thread_id, &state)
        .await?;

    assert_eq!(
        runtime
            .load_model_client_resume_state(thread_id, "account-a")
            .await?,
        Some(state)
    );
    assert_eq!(
        runtime
            .load_model_client_resume_state(thread_id, "account-b")
            .await?,
        None
    );
    assert_eq!(
        runtime
            .load_model_client_resume_state(ThreadId::new(), "account-a")
            .await?,
        None
    );
    Ok(())
}
