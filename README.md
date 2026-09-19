# c6-bank

Async Rust SDK for C6 Bank Pix. Supports due-date charges (`cobv`), received Pix queries and Pix webhook configuration. Uses Rust 2024, reqwest 0.13 and rustls mutual TLS.

```toml
[dependencies]
c6-bank = "0.1.0"
```

```rust,no_run
use c6_bank::{Client, DueChargeRequest};

# async fn example() -> Result<(), Box<dyn std::error::Error>> {
let client = Client::builder(
    std::env::var("C6_CLIENT_ID")?,
    std::env::var("C6_CLIENT_SECRET")?,
)
.identity_pem(
    &std::fs::read("client-certificate.pem")?,
    &std::fs::read("client-private-key.pem")?,
)?
.build()?; // Sandbox by default.

// Persist this ID and the full request before sending. A retry uses the same ID.
let txid = "12345678901234567890123456789012";
let request: DueChargeRequest = serde_json::from_str(r#"{
  "calendario": {"dataDeVencimento": "2026-10-01", "validadeAposVencimento": 90},
  "devedor": {
    "nome": "Example Customer", "cpf": "12345678909",
    "logradouro": "Rua Exemplo, 10", "cidade": "Recife", "uf": "PE", "cep": "50000000"
  },
  "valor": {"original": "10.01"},
  "chave": "your-registered-pix-key"
}"#)?;
match client.put_due_charge(txid, &request).await {
    Ok(charge) => println!("{}: {}", charge.txid, charge.status),
    Err(error) if error.is_indeterminate() => {
        // Read the SAME ID; compare the bank result with the persisted request.
        // If GET also fails, retain the pending attempt and reconcile later.
        let existing = client.get_due_charge(txid).await?;
        println!("Reconcile {}: {}", existing.txid, existing.status);
    }
    Err(error) => return Err(error.into()),
}
# Ok(()) }
```

Supply CNPJ instead of CPF for a legal entity. C6's inline PUT example lists CPF, while its reusable debtor schema supports CPF or CNPJ. Send exactly one. All amounts and percentages are decimal strings; the SDK never converts them through floating point. Unknown response status values and extra fields are retained. Identity fields required to reconcile charges and receipts remain required during deserialization; optional response metadata may be absent.

## Client and errors

`Client` is cloneable. Clones share the connection pool and an expiry-aware OAuth token cache. Concurrent refresh requests use one authentication call. Authentication posts `grant_type=client_credentials`, `client_id` and `client_secret` as a form to `/v1/auth/`.

`Environment::Production` explicitly selects production. `base_url`, `auth_url`, `timeout`, `add_root_certificate_pem` and `http_client` support controlled environments and testing. The default transport requires a PEM certificate chain and unencrypted PEM private key, checks server certificates, disables redirects and automatic retries, and has a 30-second timeout. An injected HTTP client replaces these transport settings: the caller must configure its mTLS identity, timeout, redirect policy and retry policy. Never use insecure certificate validation in production.

Errors deliberately omit server bodies, URLs, credentials and raw transport details. `status()` exposes the HTTP status, `retry_after()` retains the native `Retry-After` value, and `is_not_found()` identifies 404. `is_indeterminate()` flags mutation transport errors, HTTP 408/5xx and invalid success responses. Reconcile through GET using the original transaction ID. A 401 clears the shared token for the next explicit request without replaying the failed operation. Authentication failures occur before the mutation is sent.

## Operations

- `put_due_charge`, `get_due_charge`, `patch_due_charge`, `cancel_due_charge`: `/v2/pix/cobv/{txid}`. Cancellation is PATCH with `REMOVIDA_PELO_USUARIO_RECEBEDOR`.
- `get_pix`, `list_pix`: `/v2/pix/pix/{e2eid}` and `/v2/pix/pix`. `PixQuery` sends `inicio`, `fim`, optional `txid`, and `paginacao.paginaAtual` / `paginacao.itensPorPagina`. Pages start at zero. Process each page durably before advancing; use returned pagination metadata.
- `put_webhook`, `get_webhook`, `delete_webhook`: `/v2/pix/webhook/{chave}`. This is the Pix webhook API, distinct from C6's generic `/v1/webhooks` service.

The SDK does not create transaction IDs, automatically retry mutations, initiate refunds or apply business-specific settlement rules. A received webhook is a hint: fetch the receipt and charge from C6 and verify their identity, receiving key and amounts before recording payment. Preserve receipt value components and refund metadata for reconciliation.

## Sandbox and manual webhook setup

Set `C6_CLIENT_ID`, `C6_CLIENT_SECRET`, `C6_CERT_PATH`, `C6_KEY_PATH`. The helper uses sandbox unless `C6_ENV=production` is explicitly set.

```sh
# Fetch an existing charge or receipt without printing sensitive response bodies.
cargo run --example sandbox -- get-charge EXISTING_TXID
cargo run --example sandbox -- get-pix EXISTING_END_TO_END_ID

# Register and verify your HTTPS callback manually.
cargo run --example sandbox -- set-webhook REGISTERED_PIX_KEY https://your-service.example/callback
cargo run --example sandbox -- get-webhook REGISTERED_PIX_KEY
```

Before production, validate a due-charge lifecycle against C6 sandbox with your actual account credentials: create a charge with a caller-persisted ID, GET it, PATCH it, and cancel a separate unpaid test charge. Exercise payment and receipt queries, paginate a known time window, and capture the real notification contract. The supplied C6 notification schema includes an `information` field containing JSON as a string; do not assume that a callback always matches a bare Pix object. Confirm callback delivery and authorization requirements with C6. These live checks require bank access and are not substitutes for the local test suite.

## Development

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo test --features reqwest/native-tls # verify dependency feature unification
cargo package
```

Tests use loopback HTTP and a generated private CA to prove mutual TLS for both OAuth and Pix calls. They exercise token singleflight/expiry, form encoding, uncertain mutations without replay, 401 invalidation, exact payloads, escaped key paths and optional response fields. They never connect to a bank or require production secrets.

MIT licensed.
