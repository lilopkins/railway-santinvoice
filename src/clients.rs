use color_eyre::eyre::{Result, WrapErr, bail};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use tracing::{debug, info, warn};

use crate::config::{OidcConfig, SantInvoiceConfig};
use crate::models::{AccessTokenResponse, SubmissionResult, SubmissionStatus};
use crate::xml::find_xml_value;

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
            ("username", self.config.username.clone()),
            ("password", self.config.password.clone()),
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
