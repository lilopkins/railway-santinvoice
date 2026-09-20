use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use color_eyre::eyre::{Result, WrapErr};
use lettre::message::{Attachment, Mailbox, MultiPart, SinglePart, header::ContentType};
use lettre::{Message, SmtpTransport, Transport, transport::smtp::authentication::Credentials};
use tracing::{debug, info};

use crate::config::{InvoiceConfig, PdfEmailConfig};

pub fn send_invoice_email(
    config: &PdfEmailConfig,
    invoice: &InvoiceConfig,
    invoice_number: &str,
    pdf_bytes: &[u8],
) -> Result<()> {
    info!(
        invoice_number,
        pdf_bytes = pdf_bytes.len(),
        smtp_port = config.smtp_port,
        starttls = config.smtp_starttls,
        "preparing invoice email"
    );
    let filename = format!("invoice-{invoice_number}.pdf");
    let attachment = Attachment::new(filename).body(
        BASE64.decode(BASE64.encode(pdf_bytes))?,
        ContentType::parse("application/pdf").wrap_err("invalid PDF content type")?,
    );

    let email = Message::builder()
        .from(parse_mailbox(&config.sender_email)?)
        .to(parse_mailbox(&config.recipient_email)?)
        .subject(&config.subject)
        .multipart(
            MultiPart::mixed()
                .singlepart(SinglePart::plain(config.body.clone()))
                .singlepart(attachment),
        )
        .wrap_err("failed to build email message")?;

    let credentials = Credentials::new(config.smtp_username.clone(), config.smtp_password.clone());
    let transport_builder = if config.smtp_starttls {
        SmtpTransport::starttls_relay(&config.smtp_host).wrap_err_with(|| {
            format!(
                "failed to configure STARTTLS relay for {}",
                config.smtp_host
            )
        })?
    } else {
        SmtpTransport::relay(&config.smtp_host)
            .wrap_err_with(|| format!("failed to configure relay for {}", config.smtp_host))?
    };

    let mailer = transport_builder
        .port(config.smtp_port)
        .credentials(credentials)
        .build();

    debug!(invoice_number, "sending invoice email through SMTP");
    mailer.send(&email).wrap_err(format!(
        "failed to send invoice email for customer {}",
        invoice.customer_email
    ))?;

    info!(invoice_number, "sent invoice email");
    Ok(())
}

fn parse_mailbox(email: &str) -> Result<Mailbox> {
    email
        .parse()
        .wrap_err_with(|| format!("invalid email address: {email}"))
}
