use anyhow::{Context, anyhow, bail};
use chrono::{Datelike, Days, NaiveDate, Utc};
use reqwest::Url;
use std::env;

#[derive(Clone, Debug)]
pub struct RailwayConfig {
    pub graphql_url: String,
    pub auth_header_name: String,
    pub auth_scheme: String,
    pub token: String,
    pub workspace_id: Option<String>,
    pub project_id: String,
    pub project_name: String,
    pub billing_from: NaiveDate,
    pub billing_to: NaiveDate,
    pub currency: String,
}

#[derive(Clone, Debug)]
pub struct OidcConfig {
    pub token_url: String,
    pub client_id: String,
    pub client_secret: String,
    pub scope: Option<String>,
    pub audience: Option<String>,
}

#[derive(Clone, Debug)]
pub struct SantInvoiceConfig {
    pub base_url: String,
}

#[derive(Clone, Debug)]
pub struct InvoiceConfig {
    pub supplier_name: String,
    pub supplier_address: String,
    pub supplier_email: String,
    pub customer_name: String,
    pub customer_address: String,
    pub customer_email: String,
    pub payment_due_by: NaiveDate,
    pub currency: String,
    pub notes: Option<String>,
    pub service_summary: String,
    pub service_details: String,
    pub idempotency_prefix: String,
}

#[derive(Clone, Debug)]
pub struct PdfEmailConfig {
    pub recipient_email: String,
    pub sender_email: String,
    pub smtp_host: String,
    pub smtp_port: u16,
    pub smtp_username: String,
    pub smtp_password: String,
    pub smtp_starttls: bool,
    pub subject: String,
    pub body: String,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub railway: RailwayConfig,
    pub oidc: OidcConfig,
    pub santinvoice: SantInvoiceConfig,
    pub invoice: InvoiceConfig,
    pub pdf_email: Option<PdfEmailConfig>,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let railway = RailwayConfig {
            graphql_url: required_absolute_url("RAILWAY_GRAPHQL_URL")?,
            auth_header_name: optional_env("RAILWAY_AUTH_HEADER_NAME")
                .unwrap_or_else(|| "Authorization".to_string()),
            auth_scheme: optional_env("RAILWAY_AUTH_SCHEME").unwrap_or_else(|| "Bearer".to_string()),
            token: required_env("RAILWAY_TOKEN")?,
            workspace_id: optional_env("RAILWAY_WORKSPACE_ID"),
            project_id: required_env("RAILWAY_PROJECT_ID")?,
            project_name: required_env("RAILWAY_PROJECT_NAME")?,
            billing_from: first_day_of_previous_month()?,
            billing_to: last_day_of_previous_month()?,
            currency: optional_env("RAILWAY_BILLING_CURRENCY").unwrap_or_else(|| "GBP".to_string()),
        };

        if railway.billing_to < railway.billing_from {
            bail!("RAILWAY_BILLING_TO must be on or after RAILWAY_BILLING_FROM");
        }

        let oidc = OidcConfig {
            token_url: required_absolute_url("OIDC_TOKEN_URL")?,
            client_id: required_env("OIDC_CLIENT_ID")?,
            client_secret: required_env("OIDC_CLIENT_SECRET")?,
            scope: optional_env("OIDC_SCOPE"),
            audience: optional_env("OIDC_AUDIENCE"),
        };

        let santinvoice = SantInvoiceConfig {
            base_url: required_absolute_url("SANTINVOICE_BASE_URL")?,
        };

        let invoice = InvoiceConfig {
            supplier_name: required_env("INVOICE_SUPPLIER_NAME")?,
            supplier_address: required_env("INVOICE_SUPPLIER_ADDRESS")?,
            supplier_email: required_env("INVOICE_SUPPLIER_EMAIL")?,
            customer_name: required_env("INVOICE_CUSTOMER_NAME")?,
            customer_address: required_env("INVOICE_CUSTOMER_ADDRESS")?,
            customer_email: required_env("INVOICE_CUSTOMER_EMAIL")?,
            payment_due_by: parse_date_env("INVOICE_PAYMENT_DUE_BY")
                .or_else(|_| default_due_date())?,
            currency: optional_env("INVOICE_CURRENCY").unwrap_or_else(|| railway.currency.clone()),
            notes: optional_env("INVOICE_NOTES"),
            service_summary: optional_env("INVOICE_SERVICE_SUMMARY")
                .unwrap_or_else(|| format!("Railway billing for {}", railway.project_name)),
            service_details: optional_env("INVOICE_SERVICE_DETAILS")
                .unwrap_or_else(|| billing_details_default(&railway)),
            idempotency_prefix: optional_env("INVOICE_IDEMPOTENCY_PREFIX")
                .unwrap_or_else(|| "railway-santinvoice".to_string()),
        };

        let pdf_email = if env_flag("PDF_EMAIL_ENABLED") {
            Some(PdfEmailConfig {
                recipient_email: required_env("PDF_EMAIL_RECIPIENT")?,
                sender_email: required_env("SMTP_FROM_EMAIL")?,
                smtp_host: required_env("SMTP_HOST")?,
                smtp_port: parse_u16_env("SMTP_PORT").unwrap_or(587),
                smtp_username: required_env("SMTP_USERNAME")?,
                smtp_password: required_env("SMTP_PASSWORD")?,
                smtp_starttls: env_flag_default("SMTP_STARTTLS", true),
                subject: optional_env("PDF_EMAIL_SUBJECT")
                    .unwrap_or_else(|| format!("Invoice for {}", railway.project_name)),
                body: optional_env("PDF_EMAIL_BODY").unwrap_or_else(|| {
                    format!(
                        "Please find attached the invoice PDF for {}.",
                        railway.project_name
                    )
                }),
            })
        } else {
            None
        };

        Ok(Self {
            railway,
            oidc,
            santinvoice,
            invoice,
            pdf_email,
        })
    }
}

fn required_env(name: &str) -> anyhow::Result<String> {
    let value = env::var(name).with_context(|| format!("missing required environment variable {name}"))?;
    Ok(normalize_env_value(&value))
}

fn normalize_env_value(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.len() < 2 {
        return trimmed.to_string();
    }

    let first = trimmed.as_bytes()[0] as char;
    let last = trimmed.as_bytes()[trimmed.len() - 1] as char;

    if (first == '"' && last == '"') || (first == '\'' && last == '\'') {
        return trimmed[1..trimmed.len() - 1].to_string();
    }

    trimmed.to_string()
}

fn optional_env(name: &str) -> Option<String> {
    let value = env::var(name).ok()?;
    Some(normalize_env_value(&value))
}

fn required_absolute_url(name: &str) -> anyhow::Result<String> {
    let value = required_env(name)?;
    let parsed = Url::parse(&value)
        .with_context(|| format!("{name} must be an absolute URL, got {value:?}"))?;

    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        bail!("{name} must use http or https, got {:?}", parsed.scheme());
    }

    if parsed.host_str().is_none() {
        bail!("{name} must include a host, got {value:?}");
    }

    Ok(value)
}

fn parse_date_env(name: &str) -> anyhow::Result<NaiveDate> {
    let value = required_env(name)?;
    NaiveDate::parse_from_str(&value, "%Y-%m-%d")
        .with_context(|| format!("{name} must use YYYY-MM-DD format"))
}

fn parse_u16_env(name: &str) -> Option<u16> {
    optional_env(name)?.parse().ok()
}

fn env_flag(name: &str) -> bool {
    env_flag_default(name, false)
}

fn env_flag_default(name: &str, default: bool) -> bool {
    optional_env(name)
        .map(|value| {
            matches!(
                value.to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(default)
}

fn default_due_date() -> anyhow::Result<NaiveDate> {
    let today = Utc::now().date_naive();
    today
        .checked_add_days(Days::new(14))
        .ok_or_else(|| anyhow!("failed to calculate default invoice due date"))
}

fn first_day_of_previous_month() -> anyhow::Result<NaiveDate> {
    let today = Utc::now().date_naive();
    let first_day_of_current_month = NaiveDate::from_ymd_opt(today.year(), today.month(), 1)
        .ok_or_else(|| anyhow!("failed to calculate first day of current month"))?;
    let last_day_of_previous_month = first_day_of_current_month
        .checked_sub_days(Days::new(1))
        .ok_or_else(|| anyhow!("failed to calculate last day of previous month"))?;

    NaiveDate::from_ymd_opt(
        last_day_of_previous_month.year(),
        last_day_of_previous_month.month(),
        1,
    )
    .ok_or_else(|| anyhow!("failed to calculate first day of previous month"))
}

fn last_day_of_previous_month() -> anyhow::Result<NaiveDate> {
    let today = Utc::now().date_naive();
    let first_day_of_current_month = NaiveDate::from_ymd_opt(today.year(), today.month(), 1)
        .ok_or_else(|| anyhow!("failed to calculate first day of current month"))?;

    first_day_of_current_month
        .checked_sub_days(Days::new(1))
        .ok_or_else(|| anyhow!("failed to calculate last day of previous month"))
}

fn billing_details_default(config: &RailwayConfig) -> String {
    format!(
        "Summarized Railway usage charges for project {} covering {} to {}.",
        config.project_name, config.billing_from, config.billing_to
    )
}

#[cfg(test)]
mod tests {
    use super::normalize_env_value;

    #[test]
    fn normalize_env_value_strips_matching_quotes() {
        assert_eq!(
            normalize_env_value("\"https://backboard.railway.com/graphql/v2\""),
            "https://backboard.railway.com/graphql/v2"
        );
        assert_eq!(normalize_env_value("'https://example.com'"), "https://example.com");
        assert_eq!(normalize_env_value("https://example.com"), "https://example.com");
    }
}
