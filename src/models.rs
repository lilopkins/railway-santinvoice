use chrono::NaiveDate;
use color_eyre::eyre::{Result, bail};
use serde::Deserialize;
use tracing::debug;

use crate::config::Config;

#[derive(Debug)]
pub struct Invoice {
    pub idempotency_key: String,
    pub payment_due_by: NaiveDate,
    pub currency: String,
    pub supplier: Party,
    pub customer: Party,
    pub items: Vec<InvoiceItem>,
    pub notes: Option<String>,
}

impl Invoice {
    pub fn from_billing(
        config: &Config,
        billing: &ProjectBilling,
        idempotency_key: String,
    ) -> Result<Self> {
        debug!(
            project_id = %billing.project_id,
            billing_amount = billing.amount.value,
            currency = %billing.amount.currency,
            "building invoice from Railway billing"
        );
        if billing.amount.value <= 0.0 {
            bail!("Railway billing amount must be greater than zero");
        }

        let details = format!(
            "{}\nProject: {}\nProject ID: {}\nPeriod: {} to {}",
            config.invoice.service_details,
            billing.project_name,
            billing.project_id,
            billing.billing_from,
            billing.billing_to
        );

        Ok(Self {
            idempotency_key,
            payment_due_by: config.invoice.payment_due_by,
            currency: billing.amount.currency.clone(),
            supplier: Party {
                name: config.invoice.supplier_name.clone(),
                address: config.invoice.supplier_address.clone(),
                email: config.invoice.supplier_email.clone(),
            },
            customer: Party {
                name: config.invoice.customer_name.clone(),
                address: config.invoice.customer_address.clone(),
                email: config.invoice.customer_email.clone(),
            },
            items: vec![InvoiceItem {
                date: billing.billing_to,
                summary: config.invoice.service_summary.clone(),
                details: Some(details),
                link: None,
                tags: vec![
                    "railway".to_string(),
                    format!("railway-project={}", billing.project_id),
                    format!(
                        "billing-period={}..{}",
                        billing.billing_from, billing.billing_to
                    ),
                ],
                debit: billing.amount.clone(),
                credit: None,
            }],
            notes: config.invoice.notes.clone(),
        })
    }
}

#[derive(Debug)]
pub struct Party {
    pub name: String,
    pub address: String,
    pub email: String,
}

#[derive(Debug)]
pub struct InvoiceItem {
    pub date: NaiveDate,
    pub summary: String,
    pub details: Option<String>,
    pub link: Option<String>,
    pub tags: Vec<String>,
    pub debit: Money,
    pub credit: Option<Money>,
}

#[derive(Clone, Debug)]
pub struct ProjectBilling {
    pub project_id: String,
    pub project_name: String,
    pub amount: Money,
    pub billing_from: NaiveDate,
    pub billing_to: NaiveDate,
}

#[derive(Clone, Debug)]
pub struct Money {
    pub currency: String,
    pub value: f64,
}

#[derive(Debug, Deserialize)]
pub struct AccessTokenResponse {
    pub access_token: String,
    #[allow(dead_code)]
    pub token_type: Option<String>,
    #[allow(dead_code)]
    pub expires_in: Option<u64>,
}

#[derive(Debug)]
pub struct SubmissionResult {
    pub status: SubmissionStatus,
    pub invoice_number: Option<String>,
    #[allow(dead_code)]
    pub response_body: String,
}

impl SubmissionResult {
    pub const fn status_label(&self) -> &'static str {
        match self.status {
            SubmissionStatus::Created => "created",
            SubmissionStatus::Duplicate => "duplicate",
        }
    }
}

#[derive(Debug)]
pub enum SubmissionStatus {
    Created,
    Duplicate,
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;

    use super::{Invoice, Money, ProjectBilling};
    use crate::config::{Config, InvoiceConfig, OidcConfig, RailwayConfig, SantInvoiceConfig};

    #[test]
    fn invoice_currency_matches_the_railway_billing_amount() {
        let config = Config {
            railway: RailwayConfig {
                graphql_url: "https://railway.example/graphql".to_string(),
                token: "token".to_string(),
                workspace_id: "workspace".to_string(),
                project_id: "project".to_string(),
                project_name: "Project".to_string(),
                currency: "GBP".to_string(),
            },
            oidc: OidcConfig {
                token_url: "https://oidc.example/token".to_string(),
                client_id: "client".to_string(),
                client_secret: "secret".to_string(),
                scope: None,
                audience: None,
            },
            santinvoice: SantInvoiceConfig {
                base_url: "https://santinvoice.example".to_string(),
            },
            invoice: InvoiceConfig {
                supplier_name: "Supplier".to_string(),
                supplier_address: "Supplier address".to_string(),
                supplier_email: "supplier@example.com".to_string(),
                customer_name: "Customer".to_string(),
                customer_address: "Customer address".to_string(),
                customer_email: "customer@example.com".to_string(),
                payment_due_by: NaiveDate::from_ymd_opt(2026, 9, 30).unwrap(),
                notes: None,
                service_summary: "Railway billing".to_string(),
                service_details: "Usage".to_string(),
                idempotency_prefix: "test".to_string(),
            },
            pdf_email: None,
        };
        let billing = ProjectBilling {
            project_id: "project".to_string(),
            project_name: "Project".to_string(),
            amount: Money {
                currency: "USD".to_string(),
                value: 12.34,
            },
            billing_from: NaiveDate::from_ymd_opt(2026, 8, 1).unwrap(),
            billing_to: NaiveDate::from_ymd_opt(2026, 8, 31).unwrap(),
        };

        let invoice = Invoice::from_billing(&config, &billing, "key".to_string()).unwrap();

        assert_eq!(invoice.currency, "USD");
        assert_eq!(invoice.items[0].debit.currency, "USD");
    }
}
