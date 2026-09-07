use anyhow::Context;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use lettre::message::{Attachment, Mailbox, MultiPart, SinglePart, header::ContentType};
use lettre::{Message, SmtpTransport, Transport, transport::smtp::authentication::Credentials};

use crate::config::{InvoiceConfig, PdfEmailConfig};

pub fn send_invoice_email(
    config: &PdfEmailConfig,
    invoice: &InvoiceConfig,
    invoice_number: &str,
    pdf_bytes: &[u8],
) -> anyhow::Result<()> {
    let filename = format!("invoice-{invoice_number}.pdf");
    let attachment = Attachment::new(filename).body(
        BASE64.decode(BASE64.encode(pdf_bytes))?,
        ContentType::parse("application/pdf").context("invalid PDF content type")?,
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
        .context("failed to build email message")?;

    let credentials = Credentials::new(config.smtp_username.clone(), config.smtp_password.clone());
    let transport_builder = if config.smtp_starttls {
        SmtpTransport::starttls_relay(&config.smtp_host).with_context(|| {
            format!(
                "failed to configure STARTTLS relay for {}",
                config.smtp_host
            )
        })?
    } else {
        SmtpTransport::relay(&config.smtp_host)
            .with_context(|| format!("failed to configure relay for {}", config.smtp_host))?
    };

    let mailer = transport_builder
        .port(config.smtp_port)
        .credentials(credentials)
        .build();

    mailer.send(&email).context(format!(
        "failed to send invoice email for customer {}",
        invoice.customer_email
    ))?;

    Ok(())
}

fn parse_mailbox(email: &str) -> anyhow::Result<Mailbox> {
    email
        .parse()
        .with_context(|| format!("invalid email address: {email}"))
}
