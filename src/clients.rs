use anyhow::{Context, anyhow, bail};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use serde::Deserialize;
use serde_json::json;

use crate::config::{OidcConfig, RailwayConfig, SantInvoiceConfig};
use crate::models::{AccessToken, Money, ProjectBilling, SubmissionResult, SubmissionStatus};
use crate::xml::find_xml_value;

#[derive(Clone)]
pub struct RailwayClient {
    http: reqwest::Client,
    config: RailwayConfig,
}

impl RailwayClient {
    pub fn new(http: reqwest::Client, config: RailwayConfig) -> Self {
        Self { http, config }
    }

    pub async fn fetch_project_billing(&self) -> anyhow::Result<ProjectBilling> {
        let project = self.fetch_project_identity().await?;
        let total_cost = self.fetch_project_cost().await?;

        Ok(ProjectBilling {
            project_id: project.id,
            project_name: project.name,
            amount: Money {
                currency: self.config.currency.clone(),
                value: total_cost,
            },
            billing_from: self.config.billing_from,
            billing_to: self.config.billing_to,
        })
    }

    async fn fetch_project_identity(&self) -> anyhow::Result<RailwayProject> {
        let payload: GraphQlResponse<ProjectOnlyEnvelope> = self
            .post_graphql(
                r#"
                    query ProjectIdentity($projectId: String!) {
                      project(id: $projectId) {
                        id
                        name
                      }
                    }
                "#,
                json!({ "projectId": self.config.project_id }),
            )
            .await?;

        payload
            .data
            .and_then(|data| data.project)
            .ok_or_else(|| anyhow!("Railway GraphQL response did not include project data"))
    }

    async fn fetch_project_cost(&self) -> anyhow::Result<f64> {
        let queries = [
            UsageQueryAttempt {
                measurement: "COST",
                group_by: vec!["PROJECT_ID"],
            },
            UsageQueryAttempt {
                measurement: "COST",
                group_by: vec![],
            },
            UsageQueryAttempt {
                measurement: "TOTAL_COST",
                group_by: vec!["PROJECT_ID"],
            },
            UsageQueryAttempt {
                measurement: "TOTAL_COST",
                group_by: vec![],
            },
        ];

        let mut failures = Vec::new();
        for attempt in queries {
            match self.try_usage_query(&attempt).await {
                Ok(total) => return Ok(total),
                Err(error) => failures.push(error.to_string()),
            }
        }

        bail!(
            "Railway billing query failed for all known metric variants: {}",
            failures.join(" | ")
        )
    }

    async fn try_usage_query(&self, attempt: &UsageQueryAttempt<'_>) -> anyhow::Result<f64> {
        let payload: GraphQlResponse<UsageEnvelope> = self
            .post_graphql(
                r#"
                    query ProjectUsage(
                      $projectId: String!,
                      $workspaceId: String,
                      $startDate: DateTime,
                      $endDate: DateTime,
                      $measurements: [MetricMeasurement!]!,
                      $groupBy: [MetricTag]!,
                      $includeDeleted: Boolean
                    ) {
                      usage(
                        projectId: $projectId
                        workspaceId: $workspaceId
                        startDate: $startDate
                        endDate: $endDate
                        includeDeleted: $includeDeleted
                        measurements: $measurements
                        groupBy: $groupBy
                      ) {
                        measurement
                        value
                      }
                    }
                "#,
                json!({
                    "projectId": self.config.project_id,
                    "workspaceId": self.config.workspace_id,
                    "startDate": self.config.billing_from.and_hms_opt(0, 0, 0).unwrap().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
                    "endDate": self.config.billing_to.and_hms_opt(23, 59, 59).unwrap().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
                    "measurements": [attempt.measurement],
                    "groupBy": attempt.group_by,
                    "includeDeleted": false,
                }),
            )
            .await
            .with_context(|| {
                format!(
                    "usage query failed for measurement {} and groupBy {:?}",
                    attempt.measurement, attempt.group_by
                )
            })?;

        let usage_rows = payload
            .data
            .and_then(|data| data.usage)
            .ok_or_else(|| anyhow!("Railway GraphQL response did not include usage data"))?;

        usage_rows.into_iter().try_fold(0.0, |acc, row| {
            row.value
                .parse::<f64>()
                .map(|value| acc + value)
                .with_context(|| format!("invalid Railway usage value {:?}", row.value))
        })
    }

    async fn post_graphql<T: for<'de> Deserialize<'de>>(
        &self,
        query: &str,
        variables: serde_json::Value,
    ) -> anyhow::Result<GraphQlResponse<T>> {
        let auth_value = build_railway_auth_value(&self.config.token, &self.config.auth_scheme);

        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_bytes(self.config.auth_header_name.as_bytes())
                .context("invalid RAILWAY_AUTH_HEADER_NAME")?,
            HeaderValue::from_str(&auth_value).context("invalid Railway auth header value")?,
        );
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        let response = self
            .http
            .post(&self.config.graphql_url)
            .headers(headers)
            .json(&json!({ "query": query, "variables": variables }))
            .send()
            .await
            .context("failed to call Railway GraphQL API")?;

        let status = response.status();
        let body = response.text().await.with_context(|| {
            format!("failed to read Railway GraphQL response with status {status}")
        })?;

        let payload: GraphQlResponse<T> = serde_json::from_str(&body).with_context(|| {
            format!("failed to decode Railway GraphQL response with status {status}: {body}")
        })?;

        if let Some(errors) = &payload.errors {
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

fn build_railway_auth_value(token: &str, scheme: &str) -> String {
    let token = token.trim();
    let lower = token.to_ascii_lowercase();
    if lower.starts_with("bearer ") {
        return token.to_string();
    }

    let scheme = scheme.trim();
    if scheme.is_empty() {
        token.to_string()
    } else {
        format!("{} {}", scheme.trim(), token)
    }
}

#[cfg(test)]
mod tests {
    use super::build_railway_auth_value;

    #[test]
    fn railway_auth_value_handles_bare_and_prefixed_tokens() {
        assert_eq!(build_railway_auth_value("abc123", "Bearer"), "Bearer abc123");
        assert_eq!(build_railway_auth_value("Bearer abc123", "Bearer"), "Bearer abc123");
        assert_eq!(build_railway_auth_value("abc123", ""), "abc123");
    }
}

#[derive(Clone)]
pub struct OidcClient {
    http: reqwest::Client,
    config: OidcConfig,
}

impl OidcClient {
    pub fn new(http: reqwest::Client, config: OidcConfig) -> Self {
        Self { http, config }
    }

    pub async fn fetch_access_token(&self) -> anyhow::Result<AccessToken> {
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

        let response = self
            .http
            .post(&self.config.token_url)
            .form(&params)
            .send()
            .await
            .context("failed to call OIDC token endpoint")?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            bail!("OIDC token endpoint failed with status {status}: {body}");
        }

        let token: AccessToken = response
            .json()
            .await
            .context("failed to decode OIDC token response")?;

        if token.access_token.is_empty() {
            bail!("OIDC token response did not contain access_token");
        }

        Ok(token)
    }
}

#[derive(Clone)]
pub struct SantInvoiceClient {
    http: reqwest::Client,
    config: SantInvoiceConfig,
}

impl SantInvoiceClient {
    pub fn new(http: reqwest::Client, config: SantInvoiceConfig) -> Self {
        Self { http, config }
    }

    pub async fn submit_invoice(
        &self,
        access_token: &str,
        idempotency_key: &str,
        payload: &str,
    ) -> anyhow::Result<SubmissionResult> {
        let url = format!(
            "{}/submit/invoice",
            self.config.base_url.trim_end_matches('/')
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
            .context("failed to submit invoice to SantInvoice")?;

        let status = response.status();
        let body = response
            .text()
            .await
            .context("failed to read SantInvoice response body")?;

        match status.as_u16() {
            200 => Ok(SubmissionResult {
                status: SubmissionStatus::Duplicate,
                invoice_number: find_xml_value(&body, "invoiceNumber"),
                response_body: body,
            }),
            201 => Ok(SubmissionResult {
                status: SubmissionStatus::Created,
                invoice_number: find_xml_value(&body, "invoiceNumber"),
                response_body: body,
            }),
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
    ) -> anyhow::Result<Vec<u8>> {
        let url = format!(
            "{}/pdf-statement/invoice/{}",
            self.config.base_url.trim_end_matches('/'),
            invoice_number
        );
        let response = self
            .http
            .get(url)
            .header(AUTHORIZATION, format!("Bearer {access_token}"))
            .send()
            .await
            .context("failed to fetch SantInvoice PDF")?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            bail!("SantInvoice PDF request failed with status {status}: {body}");
        }

        response
            .bytes()
            .await
            .map(|bytes| bytes.to_vec())
            .context("failed to read SantInvoice PDF body")
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
struct RailwayProject {
    id: String,
    name: String,
}

#[derive(Debug, Deserialize)]
struct RailwayUsageRow {
    measurement: String,
    value: String,
}

struct UsageQueryAttempt<'a> {
    measurement: &'a str,
    group_by: Vec<&'a str>,
}
