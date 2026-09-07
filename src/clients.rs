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
        let auth_value = if self.config.auth_scheme.is_empty() {
            self.config.token.clone()
        } else {
            format!("{} {}", self.config.auth_scheme, self.config.token)
        };

        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_bytes(self.config.auth_header_name.as_bytes())
                .context("invalid RAILWAY_AUTH_HEADER_NAME")?,
            HeaderValue::from_str(&auth_value).context("invalid Railway auth header value")?,
        );
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        let query = r#"
            query ProjectInvoiceData($projectId: String!, $from: DateTime!, $to: DateTime!) {
              project(id: $projectId) {
                id
                name
                usageCosts(startDate: $from, endDate: $to) {
                  totalCost
                }
              }
            }
        "#;

        let variables = json!({
            "projectId": self.config.project_id,
            "from": self.config.billing_from.and_hms_opt(0, 0, 0).unwrap().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            "to": self.config.billing_to.and_hms_opt(23, 59, 59).unwrap().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        });

        let response = self
            .http
            .post(&self.config.graphql_url)
            .headers(headers)
            .json(&json!({ "query": query, "variables": variables }))
            .send()
            .await
            .context("failed to call Railway GraphQL API")?;

        let status = response.status();
        let payload: GraphQlResponse<RailwayProjectEnvelope> =
            response.json().await.with_context(|| {
                format!("failed to decode Railway GraphQL response with status {status}")
            })?;

        if let Some(errors) = payload.errors {
            let summary = errors
                .into_iter()
                .map(|error| error.message)
                .collect::<Vec<_>>()
                .join("; ");
            bail!("Railway GraphQL returned errors: {summary}");
        }

        let project = payload
            .data
            .and_then(|data| data.project)
            .ok_or_else(|| anyhow!("Railway GraphQL response did not include project data"))?;

        let total_cost = project
            .usage_costs
            .total_cost
            .parse::<f64>()
            .with_context(|| {
                format!(
                    "invalid Railway totalCost {:?}",
                    project.usage_costs.total_cost
                )
            })?;

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
}

#[derive(Debug, Deserialize)]
struct RailwayProjectEnvelope {
    project: Option<RailwayProject>,
}

#[derive(Debug, Deserialize)]
struct RailwayProject {
    id: String,
    name: String,
    #[serde(rename = "usageCosts")]
    usage_costs: RailwayUsageCosts,
}

#[derive(Debug, Deserialize)]
struct RailwayUsageCosts {
    #[serde(rename = "totalCost")]
    total_cost: String,
}
