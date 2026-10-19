use super::*;

const MODEL_CLIENT_RESUME_STATE_TTL_SECONDS: i64 = 30 * 60;
const MODEL_CLIENT_RESUME_STATE_RETENTION_SECONDS: i64 = 24 * 60 * 60;
const MAX_TURN_STATE_BYTES: usize = 8 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelClientResumeState {
    pub account_id: String,
    pub turn_state: String,
}

impl StateRuntime {
    pub async fn load_model_client_resume_state(
        &self,
        thread_id: ThreadId,
        account_id: &str,
    ) -> anyhow::Result<Option<ModelClientResumeState>> {
        if account_id.is_empty() {
            return Ok(None);
        }
        let fresh_after = Utc::now()
            .timestamp()
            .saturating_sub(MODEL_CLIENT_RESUME_STATE_TTL_SECONDS);
        let row = sqlx::query(
            r#"
SELECT account_id, turn_state
FROM model_client_resume_state
WHERE thread_id = ? AND account_id = ? AND updated_at >= ?
            "#,
        )
        .bind(thread_id.to_string())
        .bind(account_id)
        .bind(fresh_after)
        .fetch_optional(self.pool.as_ref())
        .await?;
        row.map(|row| {
            Ok(ModelClientResumeState {
                account_id: row.try_get("account_id")?,
                turn_state: row.try_get("turn_state")?,
            })
        })
        .transpose()
    }

    pub async fn record_model_client_resume_state(
        &self,
        thread_id: ThreadId,
        state: &ModelClientResumeState,
    ) -> anyhow::Result<()> {
        if state.account_id.is_empty()
            || state.turn_state.is_empty()
            || state.turn_state.len() > MAX_TURN_STATE_BYTES
        {
            return Ok(());
        }
        let now = Utc::now().timestamp();
        let retain_after = now.saturating_sub(MODEL_CLIENT_RESUME_STATE_RETENTION_SECONDS);
        let mut transaction = self.pool.begin().await?;
        sqlx::query(
            r#"
INSERT INTO model_client_resume_state (
    thread_id, account_id, turn_state, updated_at
) VALUES (?, ?, ?, ?)
ON CONFLICT(thread_id, account_id) DO UPDATE SET
    turn_state = excluded.turn_state,
    updated_at = excluded.updated_at
            "#,
        )
        .bind(thread_id.to_string())
        .bind(&state.account_id)
        .bind(&state.turn_state)
        .bind(now)
        .execute(&mut *transaction)
        .await?;
        sqlx::query("DELETE FROM model_client_resume_state WHERE updated_at < ?")
            .bind(retain_after)
            .execute(&mut *transaction)
            .await?;
        transaction.commit().await?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "model_client_resume_state_tests.rs"]
mod tests;
