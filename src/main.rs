#![warn(clippy::pedantic)]
#![warn(clippy::nursery)]

mod clients;
mod config;
mod email;
mod models;
mod xml;

use clients::{OidcClient, RailwayClient, SantInvoiceClient};
use color_eyre::eyre::{Result, WrapErr, eyre};
use config::Config;
use email::send_invoice_email;
use models::Invoice;
use tracing::{debug, error, info, warn};
use tracing_subscriber::EnvFilter;
use uuid::Uuid;
use xml::build_invoice_xml;

#[tokio::main]
async fn main() -> Result<()> {
    // Load from .env, ignoring error (allow config without .env file)
    let _ = dotenvy::dotenv();

    color_eyre::install()?;
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    if let Err(error) = run().await {
        error!("invoice workflow failed");
        return Err(error);
    }

    Ok(())
}

async fn run() -> Result<()> {
    info!("starting invoice workflow");
    let config = Config::from_env()?;
    debug!(
        graphql_endpoint_configured = !config.railway.graphql_url.is_empty(),
        oidc_endpoint_configured = !config.oidc.token_url.is_empty(),
        santinvoice_endpoint_configured = !config.santinvoice.base_url.is_empty(),
        "configuration loaded"
    );
    let http = reqwest::Client::builder()
        .user_agent(format!(
            "{}/{}",
            env!("CARGO_PKG_NAME"),
            env!("CARGO_PKG_VERSION")
        ))
        .build()
        .wrap_err("failed to build HTTP client")?;
    debug!("HTTP client initialized");

    let railway_client = RailwayClient::new(http.clone(), config.railway.clone());
    let billing = railway_client.fetch_project_billing().await?;
    info!(
        project_id = %billing.project_id,
        project_name = %billing.project_name,
        amount = billing.amount.value,
        currency = %billing.amount.currency,
        billing_from = %billing.billing_from,
        billing_to = %billing.billing_to,
        "retrieved Railway billing"
    );

    let oidc_client = OidcClient::new(http.clone(), config.oidc.clone());
    let token = oidc_client.fetch_access_token().await?;
    debug!(
        token_type = ?token.token_type,
        expires_in_seconds = ?token.expires_in,
        "obtained OIDC access token"
    );

    let invoice = Invoice::from_billing(
        &config,
        &billing,
        format!("{}-{}", config.invoice.idempotency_prefix, Uuid::new_v4()),
    )?;
    debug!(
        idempotency_key = %invoice.idempotency_key,
        item_count = invoice.items.len(),
        currency = %invoice.currency,
        payment_due_by = %invoice.payment_due_by,
        "created invoice model"
    );
    let payload = build_invoice_xml(&invoice)?;
    debug!(payload_bytes = payload.len(), "generated invoice XML");

    let santinvoice_client = SantInvoiceClient::new(http.clone(), config.santinvoice.clone());
    let submission = santinvoice_client
        .submit_invoice(&token.access_token, &invoice.idempotency_key, &payload)
        .await?;

    info!(
        status = submission.status_label(),
        invoice_number = ?submission.invoice_number,
        "invoice submitted"
    );
    println!(
        "invoice submitted: status={} invoice_number={}",
        submission.status_label(),
        submission.invoice_number.as_deref().unwrap_or("unknown")
    );

    if let Some(pdf_email) = &config.pdf_email {
        info!("downloading invoice PDF for email delivery");
        let post_submit = santinvoice_client
            .download_invoice_pdf(
                &token.access_token,
                submission
                    .invoice_number
                    .as_deref()
                    .ok_or_else(|| eyre!("invoice number missing from SantInvoice response"))?,
            )
            .await;

        match post_submit {
            Ok(pdf_bytes) => {
                debug!(pdf_bytes = pdf_bytes.len(), "downloaded invoice PDF");
                send_invoice_email(
                    pdf_email,
                    &config.invoice,
                    submission.invoice_number.as_deref().unwrap_or("invoice"),
                    &pdf_bytes,
                )?;
                info!("emailed invoice PDF");
                println!("pdf emailed: recipient={}", pdf_email.recipient_email);
            }
            Err(error) => {
                warn!("could not deliver invoice PDF by email after submission");
                eprintln!("post-submit warning: {error:#}");
            }
        }
    }

    info!("invoice workflow completed");
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::models::{Invoice, InvoiceItem, Money, Party};
    use crate::xml::{build_invoice_xml, find_xml_value};
    use chrono::NaiveDate;

    #[test]
    fn invoice_xml_contains_expected_elements() {
        let invoice = Invoice {
            idempotency_key: "test-key".to_string(),
            payment_due_by: NaiveDate::from_ymd_opt(2026, 1, 20).unwrap(),
            currency: "GBP".to_string(),
            supplier: Party {
                name: "Supplier".to_string(),
                address: "1 Main Street".to_string(),
                email: "supplier@example.com".to_string(),
            },
            customer: Party {
                name: "Customer".to_string(),
                address: "2 High Street".to_string(),
                email: "customer@example.com".to_string(),
            },
            items: vec![InvoiceItem {
                date: NaiveDate::from_ymd_opt(2025, 12, 31).unwrap(),
                summary: "Railway billing".to_string(),
                details: Some("Summary details".to_string()),
                link: None,
                tags: vec!["railway".to_string()],
                debit: Money {
                    currency: "GBP".to_string(),
                    value: 12.345,
                },
                credit: None,
            }],
            notes: Some("Please pay promptly".to_string()),
        };

        let xml = build_invoice_xml(&invoice).unwrap();
        assert!(xml.contains("<invoice"));
        assert!(xml.contains("<paymentDueBy>2026-01-20</paymentDueBy>"));
        assert!(xml.contains("<summary>Railway billing</summary>"));
        assert!(xml.contains("<debit>12.35</debit>"));
        assert!(xml.contains("<notes>Please pay promptly</notes>"));
    }

    #[test]
    fn xml_value_lookup_extracts_invoice_number() {
        let body = "<invoiceResponse><invoiceNumber>INV-123</invoiceNumber></invoiceResponse>";
        assert_eq!(
            find_xml_value(body, "invoiceNumber").as_deref(),
            Some("INV-123")
        );
    }
}
