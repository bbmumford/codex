use super::*;

const AUTOMATIC_WORK_WINDOW_SECONDS: i64 = 60 * 60;
const QUOTA_TELEMETRY_FRESH_SECONDS: i64 = 5 * 60;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AutomaticWorkUsageRecord {
    pub used_percent: Option<f64>,
    pub resets_at: Option<i64>,
    pub hard_limited: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AutomaticWorkClaimOutcome {
    Admitted,
    HardLimit { retry_at: Option<i64> },
    QuotaReserve { used_percent: f64 },
    HourlyBudget { retry_at: i64 },
}

impl StateRuntime {
    pub async fn record_automatic_work_usage(
        &self,
        record: AutomaticWorkUsageRecord,
    ) -> anyhow::Result<()> {
        let now = Utc::now().timestamp();
        sqlx::query(
            r#"
UPDATE automatic_work_guard
SET used_percent = COALESCE(?, used_percent),
    resets_at = COALESCE(?, resets_at),
    observed_at = ?,
    hard_limited = CASE
        WHEN ? THEN 1
        WHEN resets_at IS NOT NULL AND resets_at <= ? THEN 0
        ELSE hard_limited
    END
WHERE id = 1
            "#,
        )
        .bind(record.used_percent)
        .bind(record.resets_at)
        .bind(now)
        .bind(record.hard_limited)
        .bind(now)
        .execute(self.pool.as_ref())
        .await?;
        Ok(())
    }

    pub async fn try_claim_automatic_work(
        &self,
        maximum_claims_per_hour: i64,
        reserve_percent: f64,
    ) -> anyhow::Result<AutomaticWorkClaimOutcome> {
        let now = Utc::now().timestamp();
        let fresh_after = now.saturating_sub(QUOTA_TELEMETRY_FRESH_SECONDS);
        let window_after = now.saturating_sub(AUTOMATIC_WORK_WINDOW_SECONDS);
        let result = sqlx::query(
            r#"
UPDATE automatic_work_guard
SET hard_limited = CASE
        WHEN resets_at IS NOT NULL AND resets_at <= ? THEN 0
        ELSE hard_limited
    END,
    window_started_at = CASE
        WHEN window_started_at <= ? THEN ?
        ELSE window_started_at
    END,
    wake_count = CASE
        WHEN window_started_at <= ? THEN 1
        ELSE wake_count + 1
    END
WHERE id = 1
  AND (hard_limited = 0 OR (resets_at IS NOT NULL AND resets_at <= ?))
  AND (
      observed_at IS NULL
      OR observed_at < ?
      OR used_percent IS NULL
      OR used_percent < ?
  )
  AND (window_started_at <= ? OR wake_count < ?)
            "#,
        )
        .bind(now)
        .bind(window_after)
        .bind(now)
        .bind(window_after)
        .bind(now)
        .bind(fresh_after)
        .bind(reserve_percent)
        .bind(window_after)
        .bind(maximum_claims_per_hour)
        .execute(self.pool.as_ref())
        .await?;
        if result.rows_affected() == 1 {
            return Ok(AutomaticWorkClaimOutcome::Admitted);
        }

        let row = sqlx::query(
            r#"
SELECT used_percent, resets_at, observed_at, hard_limited,
    window_started_at, wake_count
FROM automatic_work_guard
WHERE id = 1
            "#,
        )
        .fetch_one(self.pool.as_ref())
        .await?;
        let resets_at: Option<i64> = row.try_get("resets_at")?;
        let hard_limited: bool = row.try_get("hard_limited")?;
        if hard_limited && resets_at.is_none_or(|reset| reset > now) {
            return Ok(AutomaticWorkClaimOutcome::HardLimit {
                retry_at: resets_at,
            });
        }
        let used_percent: Option<f64> = row.try_get("used_percent")?;
        let observed_at: Option<i64> = row.try_get("observed_at")?;
        if observed_at.is_some_and(|observed| observed >= fresh_after)
            && used_percent.is_some_and(|used| used >= reserve_percent)
        {
            return Ok(AutomaticWorkClaimOutcome::QuotaReserve {
                used_percent: used_percent.unwrap_or(reserve_percent),
            });
        }
        let window_started_at: i64 = row.try_get("window_started_at")?;
        Ok(AutomaticWorkClaimOutcome::HourlyBudget {
            retry_at: window_started_at.saturating_add(AUTOMATIC_WORK_WINDOW_SECONDS),
        })
    }
}

#[cfg(test)]
#[path = "automatic_work_tests.rs"]
mod tests;
