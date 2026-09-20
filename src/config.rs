use chrono::{Days, NaiveDate, Utc};
use color_eyre::eyre::{Result, WrapErr, bail, eyre};
use reqwest::Url;
use std::env;
use tracing::{debug, info};

#[derive(Clone, Debug)]
pub struct RailwayConfig {
    pub graphql_url: String,
    pub token: String,
    pub workspace_id: String,
    pub project_id: String,
    pub project_name: String,
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
    pub fn from_env() -> Result<Self> {
        let railway = RailwayConfig {
            graphql_url: required_absolute_url("RAILWAY_GRAPHQL_URL")?,
            token: required_env("RAILWAY_TOKEN")?,
            workspace_id: required_env("RAILWAY_WORKSPACE_ID")?,
            project_id: required_env("RAILWAY_PROJECT_ID")?,
            project_name: required_env("RAILWAY_TARGET_PROJECT_NAME")?,
            currency: optional_env("RAILWAY_BILLING_CURRENCY").unwrap_or_else(|| "USD".to_string()),
        };

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

        info!(
            project_id = %railway.project_id,
            workspace_id = %railway.workspace_id,
            billing_currency = %railway.currency,
            pdf_email_enabled = pdf_email.is_some(),
            "validated application configuration"
        );
        debug!(
            oidc_scope_configured = oidc.scope.is_some(),
            oidc_audience_configured = oidc.audience.is_some(),
            invoice_notes_configured = invoice.notes.is_some(),
            "configuration options resolved"
        );

        Ok(Self {
            railway,
            oidc,
            santinvoice,
            invoice,
            pdf_email,
        })
    }
}

fn required_env(name: &str) -> Result<String> {
    let value =
        env::var(name).wrap_err_with(|| format!("missing required environment variable {name}"))?;
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

fn required_absolute_url(name: &str) -> Result<String> {
    let value = required_env(name)?;
    let parsed = Url::parse(&value)
        .wrap_err_with(|| format!("{name} must be an absolute URL, got {value:?}"))?;

    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        bail!("{name} must use http or https, got {:?}", parsed.scheme());
    }

    if parsed.host_str().is_none() {
        bail!("{name} must include a host, got {value:?}");
    }

    Ok(value)
}

fn parse_date_env(name: &str) -> Result<NaiveDate> {
    let value = required_env(name)?;
    NaiveDate::parse_from_str(&value, "%Y-%m-%d")
        .wrap_err_with(|| format!("{name} must use YYYY-MM-DD format"))
}

fn parse_u16_env(name: &str) -> Option<u16> {
    optional_env(name)?.parse().ok()
}

fn env_flag(name: &str) -> bool {
    env_flag_default(name, false)
}

fn env_flag_default(name: &str, default: bool) -> bool {
    optional_env(name).map_or(default, |value| {
        matches!(
            value.to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

fn default_due_date() -> Result<NaiveDate> {
    let today = Utc::now().date_naive();
    today
        .checked_add_days(Days::new(14))
        .ok_or_else(|| eyre!("failed to calculate default invoice due date"))
}

fn billing_details_default(config: &RailwayConfig) -> String {
    format!(
        "Summarized Railway usage charges for project {}.",
        config.project_name
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
        assert_eq!(
            normalize_env_value("'https://example.com'"),
            "https://example.com"
        );
        assert_eq!(
            normalize_env_value("https://example.com"),
            "https://example.com"
        );
    }
}
