use chrono::NaiveDate;
use serde::Deserialize;

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
    ) -> anyhow::Result<Self> {
        if billing.amount.value <= 0.0 {
            anyhow::bail!("Railway billing amount must be greater than zero");
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
            currency: config.invoice.currency.clone(),
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
pub struct AccessToken {
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
    pub fn status_label(&self) -> &'static str {
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
