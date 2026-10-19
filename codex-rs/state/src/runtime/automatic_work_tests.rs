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
async fn automatic_work_claims_are_bounded_across_runtime_handles() -> anyhow::Result<()> {
    let runtime = runtime().await?;

    assert_eq!(
        runtime
            .try_claim_automatic_work(
                /*maximum_claims_per_hour*/ 2, /*reserve_percent*/ 90.0,
            )
            .await?,
        AutomaticWorkClaimOutcome::Admitted
    );
    assert_eq!(
        runtime
            .try_claim_automatic_work(
                /*maximum_claims_per_hour*/ 2, /*reserve_percent*/ 90.0,
            )
            .await?,
        AutomaticWorkClaimOutcome::Admitted
    );
    assert!(matches!(
        runtime
            .try_claim_automatic_work(
                /*maximum_claims_per_hour*/ 2, /*reserve_percent*/ 90.0,
            )
            .await?,
        AutomaticWorkClaimOutcome::HourlyBudget { .. }
    ));
    Ok(())
}

#[tokio::test]
async fn automatic_work_respects_reserve_and_sticky_hard_limit() -> anyhow::Result<()> {
    let runtime = runtime().await?;
    runtime
        .record_automatic_work_usage(AutomaticWorkUsageRecord {
            used_percent: Some(91.0),
            resets_at: Some(Utc::now().timestamp() + 3600),
            hard_limited: false,
        })
        .await?;
    assert_eq!(
        runtime
            .try_claim_automatic_work(
                /*maximum_claims_per_hour*/ 12, /*reserve_percent*/ 90.0,
            )
            .await?,
        AutomaticWorkClaimOutcome::QuotaReserve { used_percent: 91.0 }
    );

    let reset = Utc::now().timestamp() + 7200;
    runtime
        .record_automatic_work_usage(AutomaticWorkUsageRecord {
            used_percent: Some(100.0),
            resets_at: Some(reset),
            hard_limited: true,
        })
        .await?;
    runtime
        .record_automatic_work_usage(AutomaticWorkUsageRecord {
            used_percent: Some(10.0),
            resets_at: Some(reset),
            hard_limited: false,
        })
        .await?;
    assert_eq!(
        runtime
            .try_claim_automatic_work(
                /*maximum_claims_per_hour*/ 12, /*reserve_percent*/ 90.0,
            )
            .await?,
        AutomaticWorkClaimOutcome::HardLimit {
            retry_at: Some(reset)
        }
    );
    Ok(())
}
