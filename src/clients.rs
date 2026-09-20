use chrono::{DateTime, Datelike, NaiveDate, Utc};
use color_eyre::eyre::{Result, WrapErr, bail, eyre};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use serde::Deserialize;
use serde_json::json;
use tracing::{debug, info, warn};

use crate::config::{OidcConfig, RailwayConfig, SantInvoiceConfig};
use crate::models::{
    AccessTokenResponse, Money, ProjectBilling, SubmissionResult, SubmissionStatus,
};
use crate::xml::find_xml_value;

#[derive(Clone)]
pub struct RailwayClient {
    http: reqwest::Client,
    config: RailwayConfig,
}

impl RailwayClient {
    pub const fn new(http: reqwest::Client, config: RailwayConfig) -> Self {
        Self { http, config }
    }

    pub async fn fetch_project_billing(&self) -> Result<ProjectBilling> {
        info!(
            project_id = %self.config.project_id,
            workspace_id = %self.config.workspace_id,
            "fetching Railway project billing for the previous completed billing period"
        );
        let project = self.fetch_project_identity().await?;
        let billing_period = self.fetch_previous_billing_period().await?;
        let total_cost = self.fetch_project_cost(&billing_period).await?;

        debug!(
            project_id = %project.id,
            project_name = %project.name,
            total_cost,
            currency = %self.config.currency,
            "calculated Railway project billing"
        );
        Ok(ProjectBilling {
            project_id: project.id,
            project_name: project.name,
            amount: Money {
                currency: self.config.currency.clone(),
                value: total_cost,
            },
            billing_from: billing_period.from,
            billing_to: billing_period.to,
        })
    }

    async fn fetch_project_identity(&self) -> Result<RailwayProject> {
        debug!(project_id = %self.config.project_id, "requesting Railway project identity");
        let payload: GraphQlResponse<ProjectOnlyEnvelope> = self
            .post_graphql(
                "ProjectIdentity",
                r"
                    query ProjectIdentity($projectId: String!) {
                      project(id: $projectId) {
                        id
                        name
                      }
                    }
                ",
                json!({ "projectId": self.config.project_id }),
            )
            .await?;

        payload
            .data
            .and_then(|data| data.project)
            .ok_or_else(|| eyre!("Railway GraphQL response did not include project data"))
    }

    async fn fetch_previous_billing_period(&self) -> Result<BillingPeriod> {
        debug!(
            workspace_id = %self.config.workspace_id,
            "requesting Railway billing period"
        );
        let payload: GraphQlResponse<WorkspaceEnvelope> = self
            .post_graphql(
                "WorkspaceBillingPeriod",
                r"
                    query WorkspaceBillingPeriod($workspaceId: String!) {
                      workspace(workspaceId: $workspaceId) {
                        customer {
                          billingPeriod {
                            start
                          }
                        }
                      }
                    }
                ",
                json!({ "workspaceId": self.config.workspace_id }),
            )
            .await
            .wrap_err("Railway billing period query failed")?;

        let current_period_start = payload
            .data
            .and_then(|data| data.workspace)
            .map(|workspace| workspace.customer.billing_period.start)
            .ok_or_else(|| {
                eyre!("Railway GraphQL response did not include workspace billing data")
            })?;
        previous_billing_period(&current_period_start)
    }

    async fn fetch_project_cost(&self, billing_period: &BillingPeriod) -> Result<f64> {
        debug!(
            project_id = %self.config.project_id,
            workspace_id = %self.config.workspace_id,
            billing_from = %billing_period.from,
            billing_to = %billing_period.to,
            "requesting Railway usage for the previous billing period"
        );
        let payload: GraphQlResponse<UsageEnvelope> = self
            .post_graphql(
                "ProjectUsage",
                r"
                    query ProjectUsage(
                      $workspaceId: String!,
                      $startDate: DateTime!,
                      $endDate: DateTime!,
                      $measurements: [MetricMeasurement!]!
                    ) {
                      usage(
                        workspaceId: $workspaceId
                        startDate: $startDate
                        endDate: $endDate
                        includeDeleted: true
                        measurements: $measurements
                        groupBy: [PROJECT_ID]
                      ) {
                        measurement
                        value
                        tags {
                          projectId
                        }
                      }
                    }
                ",
                json!({
                    "workspaceId": self.config.workspace_id,
                    "startDate": billing_period.start,
                    "endDate": billing_period.end,
                    "measurements": BILLABLE_USAGE_MEASUREMENTS,
                }),
            )
            .await
            .wrap_err("Railway usage query failed")?;

        let usage_rows = payload
            .data
            .and_then(|data| data.usage)
            .ok_or_else(|| eyre!("Railway GraphQL response did not include usage data"))?;

        let project_usage = usage_rows
            .into_iter()
            .filter(|row| row.tags.project_id.as_deref() == Some(&self.config.project_id))
            .collect::<Vec<_>>();
        let total_cost = cost_for_usage(&project_usage);
        debug!(
            usage_row_count = project_usage.len(),
            total_cost, "calculated Railway usage cost"
        );
        Ok(total_cost)
    }

    async fn post_graphql<T: for<'de> Deserialize<'de>>(
        &self,
        operation: &str,
        query: &str,
        variables: serde_json::Value,
    ) -> Result<GraphQlResponse<T>> {
        debug!(operation, "sending Railway GraphQL request");
        let auth_value = build_railway_workspace_token_header(&self.config.token)?;

        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&auth_value).wrap_err("invalid Railway auth header value")?,
        );
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        let response = self
            .http
            .post(&self.config.graphql_url)
            .headers(headers)
            .json(&json!({ "query": query, "variables": variables }))
            .send()
            .await
            .wrap_err("failed to call Railway GraphQL API")?;

        let status = response.status();
        let body = response.text().await.wrap_err_with(|| {
            format!("failed to read Railway GraphQL response with status {status}")
        })?;
        debug!(
            operation,
            status = %status,
            response_bytes = body.len(),
            "received Railway GraphQL response"
        );

        let payload: GraphQlResponse<T> = serde_json::from_str(&body).wrap_err_with(|| {
            format!("failed to decode Railway GraphQL response with status {status}: {body}")
        })?;

        if let Some(errors) = &payload.errors {
            warn!(
                operation,
                error_count = errors.len(),
                "Railway GraphQL response contains errors"
            );
            let summary = errors
                .iter()
                .map(
                    |error| match (&error.extensions.code, &error.extensions.trace_id) {
                        (Some(code), Some(trace_id)) => {
                            format!("{} (code: {code}, traceId: {trace_id})", error.message)
                        }
                        (Some(code), None) => format!("{} (code: {code})", error.message),
                        (None, Some(trace_id)) => {
                            format!("{} (traceId: {trace_id})", error.message)
                        }
                        (None, None) => error.message.clone(),
                    },
                )
                .collect::<Vec<_>>()
                .join("; ");
            bail!("Railway GraphQL returned errors: {summary}");
        }

        Ok(payload)
    }
}

const BILLABLE_USAGE_MEASUREMENTS: [&str; 5] = [
    "MEMORY_USAGE_GB",
    "CPU_USAGE",
    "NETWORK_TX_GB",
    "DISK_USAGE_GB",
    "BACKUP_USAGE_GB",
];

const MINUTES_IN_MONTH: f64 = 43_200.0;
const MEMORY_GB_MINUTE_PRICE: f64 = 10.0 / MINUTES_IN_MONTH;
const VCPU_MINUTE_PRICE: f64 = 20.0 / MINUTES_IN_MONTH;
const EGRESS_GB_PRICE: f64 = 0.05;
const DISK_GB_MINUTE_PRICE: f64 = 0.15 / MINUTES_IN_MONTH;

fn cost_for_usage(rows: &[RailwayUsageRow]) -> f64 {
    rows.iter()
        .map(|row| match row.measurement.as_str() {
            "MEMORY_USAGE_GB" => row.value * MEMORY_GB_MINUTE_PRICE,
            "CPU_USAGE" => row.value * VCPU_MINUTE_PRICE,
            "NETWORK_TX_GB" => row.value * EGRESS_GB_PRICE,
            "DISK_USAGE_GB" | "BACKUP_USAGE_GB" => row.value * DISK_GB_MINUTE_PRICE,
            _ => 0.0,
        })
        .sum()
}

fn build_railway_workspace_token_header(token: &str) -> Result<String> {
    let token = token.trim();
    if token.to_ascii_lowercase().starts_with("bearer ") {
        bail!("RAILWAY_TOKEN must not include a Bearer prefix for a Railway project token");
    }
    Ok(format!("Bearer {token}"))
}

#[cfg(test)]
fn build_railway_project_token_header(token: &str) -> Result<String> {
    build_railway_workspace_token_header(token)
}

#[cfg(test)]
mod tests {
    use super::{
        RailwayUsageRow, RailwayUsageTags, build_railway_project_token_header,
        build_railway_workspace_token_header, cost_for_usage, previous_billing_period,
    };

    #[test]
    fn railway_workspace_token_header_adds_bearer_prefix() {
        assert_eq!(
            build_railway_workspace_token_header("abc123").unwrap(),
            "Bearer abc123"
        );
    }

    #[test]
    fn railway_workspace_token_header_rejects_bearer_prefix() {
        assert!(build_railway_project_token_header("Bearer abc123").is_err());
    }

    #[test]
    fn railway_usage_cost_matches_railway_pricing() {
        let usage = [
            RailwayUsageRow {
                measurement: "MEMORY_USAGE_GB".to_string(),
                value: 43_200.0,
                tags: RailwayUsageTags { project_id: None },
            },
            RailwayUsageRow {
                measurement: "CPU_USAGE".to_string(),
                value: 43_200.0,
                tags: RailwayUsageTags { project_id: None },
            },
            RailwayUsageRow {
                measurement: "NETWORK_TX_GB".to_string(),
                value: 2.0,
                tags: RailwayUsageTags { project_id: None },
            },
            RailwayUsageRow {
                measurement: "DISK_USAGE_GB".to_string(),
                value: 43_200.0,
                tags: RailwayUsageTags { project_id: None },
            },
            RailwayUsageRow {
                measurement: "BACKUP_USAGE_GB".to_string(),
                value: 43_200.0,
                tags: RailwayUsageTags { project_id: None },
            },
        ];

        assert!((cost_for_usage(&usage) - 30.4).abs() < f64::EPSILON);
    }

    #[test]
    fn previous_billing_period_uses_railway_billing_boundary() {
        let period = previous_billing_period("2026-09-08T00:00:00Z").unwrap();

        assert_eq!(period.start, "2026-08-08T00:00:00+00:00");
        assert_eq!(period.end, "2026-09-08T00:00:00+00:00");
        assert_eq!(period.from.to_string(), "2026-08-08");
        assert_eq!(period.to.to_string(), "2026-09-07");
    }
}

#[derive(Clone)]
pub struct OidcClient {
    http: reqwest::Client,
    config: OidcConfig,
}

impl OidcClient {
    pub const fn new(http: reqwest::Client, config: OidcConfig) -> Self {
        Self { http, config }
    }

    pub async fn fetch_access_token(&self) -> Result<AccessTokenResponse> {
        info!("requesting OIDC access token");
        let mut params = vec![
            ("grant_type", "client_credentials".to_string()),
            ("client_id", self.config.client_id.clone()),
            ("client_secret", self.config.client_secret.clone()),
        ];

        if let Some(scope) = &self.config.scope {
            params.push(("scope", scope.clone()));
        }
        if let Some(audience) = &self.config.audience {
            params.push(("audience", audience.clone()));
        }
        debug!(
            scope_configured = self.config.scope.is_some(),
            audience_configured = self.config.audience.is_some(),
            "sending OIDC client-credentials request"
        );

        let response = self
            .http
            .post(&self.config.token_url)
            .form(&params)
            .send()
            .await
            .wrap_err("failed to call OIDC token endpoint")?;

        let status = response.status();
        debug!(status = %status, "received OIDC token response");
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            warn!(
                status = %status,
                response_bytes = body.len(),
                "OIDC token request was rejected"
            );
            bail!("OIDC token endpoint failed with status {status}: {body}");
        }

        let token: AccessTokenResponse = response
            .json()
            .await
            .wrap_err("failed to decode OIDC token response")?;

        if token.access_token.is_empty() {
            bail!("OIDC token response did not contain access_token");
        }

        debug!(
            token_type = ?token.token_type,
            expires_in_seconds = ?token.expires_in,
            "OIDC access token response validated"
        );
        Ok(token)
    }
}

#[derive(Clone)]
pub struct SantInvoiceClient {
    http: reqwest::Client,
    config: SantInvoiceConfig,
}

impl SantInvoiceClient {
    pub const fn new(http: reqwest::Client, config: SantInvoiceConfig) -> Self {
        Self { http, config }
    }

    pub async fn submit_invoice(
        &self,
        access_token: &str,
        idempotency_key: &str,
        payload: &str,
    ) -> Result<SubmissionResult> {
        let url = format!(
            "{}/submit/invoice",
            self.config.base_url.trim_end_matches('/')
        );
        info!(
            idempotency_key,
            payload_bytes = payload.len(),
            "submitting invoice to SantInvoice"
        );
        let response = self
            .http
            .post(url)
            .header(AUTHORIZATION, format!("Bearer {access_token}"))
            .header("X-Idempotency-Key", idempotency_key)
            .header(CONTENT_TYPE, "text/xml")
            .body(payload.to_owned())
            .send()
            .await
            .wrap_err("failed to submit invoice to SantInvoice")?;

        let status = response.status();
        let body = response
            .text()
            .await
            .wrap_err("failed to read SantInvoice response body")?;
        debug!(
            status = %status,
            response_bytes = body.len(),
            "received SantInvoice submission response"
        );

        match status.as_u16() {
            200 => {
                let invoice_number = find_xml_value(&body, "processedDetails")
                    .and_then(|pd| find_xml_value(&pd, "number"));
                info!(invoice_number = ?invoice_number, "SantInvoice returned an existing invoice");
                Ok(SubmissionResult {
                    status: SubmissionStatus::Duplicate,
                    invoice_number,
                    response_body: body,
                })
            }
            201 => {
                let invoice_number = find_xml_value(&body, "processedDetails")
                    .and_then(|pd| find_xml_value(&pd, "number"));
                info!(invoice_number = ?invoice_number, "SantInvoice created invoice");
                Ok(SubmissionResult {
                    status: SubmissionStatus::Created,
                    invoice_number,
                    response_body: body,
                })
            }
            400 => bail!("SantInvoice rejected the XML syntax: {body}"),
            401 | 403 => bail!("SantInvoice authorization failed: {body}"),
            422 => bail!("SantInvoice rejected the invoice semantics: {body}"),
            _ => bail!("SantInvoice returned unexpected status {status}: {body}"),
        }
    }

    pub async fn download_invoice_pdf(
        &self,
        access_token: &str,
        invoice_number: &str,
    ) -> Result<Vec<u8>> {
        let url = format!(
            "{}/pdf-statement/invoice/{}",
            self.config.base_url.trim_end_matches('/'),
            invoice_number
        );
        info!(invoice_number, "downloading SantInvoice PDF");
        let response = self
            .http
            .get(url)
            .header(AUTHORIZATION, format!("Bearer {access_token}"))
            .send()
            .await
            .wrap_err("failed to fetch SantInvoice PDF")?;

        let status = response.status();
        debug!(invoice_number, status = %status, "received SantInvoice PDF response");
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            warn!(
                invoice_number,
                status = %status,
                response_bytes = body.len(),
                "SantInvoice PDF request failed"
            );
            bail!("SantInvoice PDF request failed with status {status}: {body}");
        }

        response
            .bytes()
            .await
            .map(|bytes| {
                debug!(
                    invoice_number,
                    pdf_bytes = bytes.len(),
                    "downloaded SantInvoice PDF"
                );
                bytes.to_vec()
            })
            .wrap_err("failed to read SantInvoice PDF body")
    }
}

#[derive(Debug, Deserialize)]
struct GraphQlResponse<T> {
    data: Option<T>,
    errors: Option<Vec<GraphQlError>>,
}

#[derive(Debug, Deserialize)]
struct GraphQlError {
    message: String,
    #[serde(default)]
    extensions: GraphQlErrorExtensions,
}

#[derive(Debug, Default, Deserialize)]
struct GraphQlErrorExtensions {
    #[serde(rename = "code")]
    code: Option<String>,
    #[serde(rename = "traceId")]
    trace_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ProjectOnlyEnvelope {
    project: Option<RailwayProject>,
}

#[derive(Debug, Deserialize)]
struct UsageEnvelope {
    usage: Option<Vec<RailwayUsageRow>>,
}

#[derive(Debug, Deserialize)]
struct WorkspaceEnvelope {
    workspace: Option<RailwayWorkspace>,
}

#[derive(Debug, Deserialize)]
struct RailwayWorkspace {
    customer: RailwayCustomer,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RailwayCustomer {
    billing_period: RailwayBillingPeriod,
}

#[derive(Debug, Deserialize)]
struct RailwayBillingPeriod {
    start: String,
}

#[derive(Debug, Deserialize)]
struct RailwayProject {
    id: String,
    name: String,
}

#[derive(Debug, Deserialize)]
struct RailwayUsageRow {
    measurement: String,
    value: f64,
    tags: RailwayUsageTags,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RailwayUsageTags {
    project_id: Option<String>,
}

#[derive(Debug)]
struct BillingPeriod {
    start: String,
    end: String,
    from: NaiveDate,
    to: NaiveDate,
}

fn previous_billing_period(current_start: &str) -> Result<BillingPeriod> {
    let current_start = DateTime::parse_from_rfc3339(current_start)
        .wrap_err("Railway returned an invalid billing-period start")?
        .with_timezone(&Utc);
    let previous_start = shift_months(current_start, -1)?;
    let billing_to = current_start
        .date_naive()
        .pred_opt()
        .ok_or_else(|| eyre!("failed to calculate previous billing-period end"))?;

    Ok(BillingPeriod {
        start: previous_start.to_rfc3339(),
        end: current_start.to_rfc3339(),
        from: previous_start.date_naive(),
        to: billing_to,
    })
}

fn shift_months(date: DateTime<Utc>, offset: i32) -> Result<DateTime<Utc>> {
    let naive = date.naive_utc();
    let month_index = naive.date().year() * 12 + naive.date().month0() as i32 + offset;
    let year = month_index.div_euclid(12);
    let month = (month_index.rem_euclid(12) + 1) as u32;
    let day = naive.date().day().min(days_in_month(year, month));
    let shifted = NaiveDate::from_ymd_opt(year, month, day)
        .ok_or_else(|| eyre!("failed to calculate previous billing-period start"))?
        .and_time(naive.time());

    Ok(DateTime::from_naive_utc_and_offset(shifted, Utc))
}

fn days_in_month(year: i32, month: u32) -> u32 {
    let (next_year, next_month) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    NaiveDate::from_ymd_opt(next_year, next_month, 1)
        .expect("valid next-month date")
        .pred_opt()
        .expect("month has at least one day")
        .day()
}
