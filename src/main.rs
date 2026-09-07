mod clients;
mod config;
mod email;
mod models;
mod xml;

use anyhow::{Context, anyhow};
use clients::{OidcClient, RailwayClient, SantInvoiceClient};
use config::Config;
use email::send_invoice_email;
use models::Invoice;
use uuid::Uuid;
use xml::build_invoice_xml;

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}

async fn run() -> anyhow::Result<()> {
    let config = Config::from_env()?;
    let http = reqwest::Client::builder()
        .user_agent("railway-santinvoice/0.1.0")
        .build()
        .context("failed to build HTTP client")?;

    let railway_client = RailwayClient::new(http.clone(), config.railway.clone());
    let billing = railway_client.fetch_project_billing().await?;

    let oidc_client = OidcClient::new(http.clone(), config.oidc.clone());
    let token = oidc_client.fetch_access_token().await?;

    let invoice = Invoice::from_billing(
        &config,
        &billing,
        format!("{}-{}", config.invoice.idempotency_prefix, Uuid::new_v4()),
    )?;
    let payload = build_invoice_xml(&invoice)?;

    let santinvoice_client = SantInvoiceClient::new(http.clone(), config.santinvoice.clone());
    let submission = santinvoice_client
        .submit_invoice(&token.access_token, &invoice.idempotency_key, &payload)
        .await?;

    println!(
        "invoice submitted: status={} invoice_number={}",
        submission.status_label(),
        submission.invoice_number.as_deref().unwrap_or("unknown")
    );

    if let Some(pdf_email) = &config.pdf_email {
        let post_submit = santinvoice_client
            .download_invoice_pdf(
                &token.access_token,
                submission
                    .invoice_number
                    .as_deref()
                    .ok_or_else(|| anyhow!("invoice number missing from SantInvoice response"))?,
            )
            .await;

        match post_submit {
            Ok(pdf_bytes) => {
                send_invoice_email(
                    pdf_email,
                    &config.invoice,
                    submission.invoice_number.as_deref().unwrap_or("invoice"),
                    &pdf_bytes,
                )?;
                println!("pdf emailed: recipient={}", pdf_email.recipient_email);
            }
            Err(error) => {
                eprintln!("post-submit warning: {error:#}");
            }
        }
    }

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
