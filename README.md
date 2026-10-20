# railway-santinvoice

One-shot Rust CLI that reads all configuration from environment variables, fetches Railway billing for a project, retrieves an OIDC client-credentials token, builds a SantInvoice XML invoice, submits it, and can optionally fetch a PDF copy and email it over SMTP.

## Flow

1. Read required configuration from environment variables.
2. Run the Railway CLI (`railway usage projects --period previous --json`) for the configured project's previous billing period.
3. Request an OAuth access token from the configured OIDC token endpoint.
4. Build a SantInvoice XML invoice with a single summarized line item for the billing period.
5. Submit the invoice to SantInvoice with an idempotency key.
6. Optionally download the invoice PDF and email it. PDF/email errors are reported as warnings after a successful invoice submission.

## Required environment variables

| Variable | Purpose |
| --- | --- |
| `RAILWAY_TOKEN` | Railway API token, passed to the CLI as `RAILWAY_API_TOKEN`. |
| `RAILWAY_TARGET_PROJECT_ID` | Railway project identifier to bill. |
| `RAILWAY_TARGET_PROJECT_NAME` | Human-readable project name used in invoice text. |
| `OIDC_TOKEN_URL` | OIDC token endpoint for client-credentials access tokens. |
| `OIDC_CLIENT_ID` | OIDC client ID. |
| `OIDC_USERNAME` | OIDC M2M account username. |
| `OIDC_PASSWORD` | OIDC M2M account password. |
| `SANTINVOICE_BASE_URL` | SantInvoice API base URL. |
| `INVOICE_SUPPLIER_NAME` | Supplier name for `<parties><us>`. |
| `INVOICE_SUPPLIER_ADDRESS` | Supplier address for `<parties><us>`. |
| `INVOICE_SUPPLIER_EMAIL` | Supplier email for `<parties><us>`. |
| `INVOICE_CUSTOMER_NAME` | Customer name for `<parties><them>`. |
| `INVOICE_CUSTOMER_ADDRESS` | Customer address for `<parties><them>`. |
| `INVOICE_CUSTOMER_EMAIL` | Customer email for `<parties><them>`. |

## Optional environment variables

| Variable | Default |
| --- | --- |
| `RAILWAY_BILLING_CURRENCY` | `GBP` |
| `OIDC_SCOPE` | unset |
| `OIDC_AUDIENCE` | unset |
| `INVOICE_PAYMENT_DUE_BY` | 14 days from run date |
| `INVOICE_NOTES` | unset |
| `INVOICE_SERVICE_SUMMARY` | `Railway billing for {RAILWAY_TARGET_PROJECT_NAME}` |
| `INVOICE_SERVICE_DETAILS` | Generated summary including project and date range |
| `INVOICE_IDEMPOTENCY_PREFIX` | `railway-santinvoice` (key is `<prefix>-<project_id>-<billing_year>-<billing_month>`) |
| `PDF_EMAIL_ENABLED` | `false` |
| `PDF_EMAIL_RECIPIENT` | required when PDF email is enabled |
| `SMTP_FROM_EMAIL` | required when PDF email is enabled |
| `SMTP_HOST` | required when PDF email is enabled |
| `SMTP_PORT` | `587` |
| `SMTP_USERNAME` | required when PDF email is enabled |
| `SMTP_PASSWORD` | required when PDF email is enabled |
| `SMTP_STARTTLS` | `true` |
| `PDF_EMAIL_SUBJECT` | `Invoice for {RAILWAY_TARGET_PROJECT_NAME}` |
| `PDF_EMAIL_BODY` | Simple attached-invoice message |

## Run

```bash
cargo run
```

To fetch live Railway billing and generate the invoice without requesting an
OAuth token, submitting the invoice, downloading a PDF, or sending email, use
dry-run mode:

```bash
cargo run -- --dry-run > invoice.xml
```

The generated XML is written to stdout; logs remain on stderr. The same flag
can be passed to the container entrypoint:

```bash
docker run --rm --env-file .env railway-santinvoice --dry-run > invoice.xml
```

## Logging

The CLI emits structured `tracing` logs to stderr. It defaults to `info` level; use
`RUST_LOG=debug` for request lifecycle, configuration-state, billing-calculation,
XML-size, and PDF/email-delivery diagnostics:

```bash
RUST_LOG=debug cargo run
```

Logs intentionally exclude access tokens, client secrets, SMTP credentials, XML
contents, and API response bodies.

## Container image

Build the image:

```bash
docker build -t railway-santinvoice .
```

Run it as a one-shot container with environment variables supplied by your scheduler or orchestration platform:

```bash
docker run --rm --env-file .env railway-santinvoice
```

The image is intended for cronjob-style execution in container platforms such as Kubernetes `CronJob`, Nomad periodic jobs, or any scheduler that starts a fresh container for each run.

## Behavior notes

- The app is intentionally configured only through environment variables.
- Railway data is fetched by the `railway` CLI, which must be on `PATH` (it is installed in the container image).
- The billable total is the CLI's `currentUsageDollars` for the previous billing period.
- The SantInvoice default currency is always the currency of the Railway billing amount.
- SantInvoice submission is the primary success condition.
- PDF download and SMTP delivery are best-effort follow-up steps once submission succeeds.
- The app invoices the previous billing period reported by the Railway CLI for the configured project, as one summarized invoice line item.
