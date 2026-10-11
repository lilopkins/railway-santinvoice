use chrono::{DateTime, NaiveDate};
use color_eyre::eyre::{Result, WrapErr, bail, eyre};
use serde::Deserialize;
use tokio::process::Command;
use tracing::{debug, info, warn};

use crate::config::RailwayConfig;
use crate::models::{Money, ProjectBilling};

#[derive(Clone)]
pub struct RailwayClient {
    config: RailwayConfig,
}

impl RailwayClient {
    pub const fn new(config: RailwayConfig) -> Self {
        Self { config }
    }

    pub async fn fetch_project_billing(&self) -> Result<ProjectBilling> {
        info!(
            project_id = %self.config.project_id,
            "fetching Railway project billing for the previous billing period using the Railway CLI"
        );
        let output = Command::new("railway")
            .args([
                "usage",
                "projects",
                "--project",
                &self.config.project_id,
                "--period",
                "previous",
                "--json",
            ])
            // The CLI treats RAILWAY_TOKEN as a project token and prefers it over
            // RAILWAY_API_TOKEN, so don't let it be inherited.
            .env_remove("RAILWAY_TOKEN")
            .env("RAILWAY_API_TOKEN", &self.config.token)
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .output()
            .await
            .wrap_err("failed to run the `railway` CLI; is it installed and on PATH?")?;

        if !output.status.success() {
            // With --json the CLI reports errors as JSON on stdout.
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            let detail = if stderr.trim().is_empty() {
                stdout.trim()
            } else {
                stderr.trim()
            };
            warn!(status = %output.status, "railway CLI failed");
            bail!("railway CLI exited with {}: {detail}", output.status);
        }

        let billing = parse_usage(&output.stdout, &self.config.currency)?;
        debug!(
            project_id = %billing.project_id,
            total_cost = billing.amount.value,
            "parsed Railway CLI usage"
        );
        Ok(billing)
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UsageResponse {
    billing_period: UsagePeriod,
    current_usage_dollars: f64,
    project: UsageProject,
}

#[derive(Debug, Deserialize)]
struct UsagePeriod {
    start: String,
    end: String,
}

#[derive(Debug, Deserialize)]
struct UsageProject {
    id: String,
    name: String,
}

fn parse_usage(json: &[u8], currency: &str) -> Result<ProjectBilling> {
    let usage: UsageResponse =
        serde_json::from_slice(json).wrap_err("failed to parse railway CLI JSON output")?;
    let from = parse_date(&usage.billing_period.start)?;
    let end = parse_date(&usage.billing_period.end)?;
    let to = end
        .pred_opt()
        .ok_or_else(|| eyre!("failed to calculate billing-period end"))?;

    Ok(ProjectBilling {
        project_id: usage.project.id,
        project_name: usage.project.name,
        amount: Money {
            currency: currency.to_string(),
            value: usage.current_usage_dollars,
        },
        billing_from: from,
        billing_to: to,
    })
}

fn parse_date(value: &str) -> Result<NaiveDate> {
    Ok(DateTime::parse_from_rfc3339(value)
        .wrap_err("railway CLI returned an invalid billing-period timestamp")?
        .date_naive())
}

#[cfg(test)]
mod tests {
    use super::parse_usage;

    #[test]
    fn parses_cli_usage_output() {
        let json = br#"{
            "billingPeriod": {"end": "2026-10-08T22:56:23+00:00", "start": "2026-09-08T22:56:23+00:00"},
            "currentUsageDollars": 11.87,
            "period": "previous",
            "project": {"deletedAt": null, "id": "p1", "name": "mg-fivem"},
            "services": []
        }"#;
        let billing = parse_usage(json, "USD").unwrap();

        assert_eq!(billing.project_id, "p1");
        assert_eq!(billing.project_name, "mg-fivem");
        assert!((billing.amount.value - 11.87).abs() < f64::EPSILON);
        assert_eq!(billing.billing_from.to_string(), "2026-09-08");
        assert_eq!(billing.billing_to.to_string(), "2026-10-07");
    }
}
