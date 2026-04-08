# solana-trader-quic-client-rust

bloXroute Rust client for submitting Solana transactions over QUIC using client certificate authentication.

## Quick Start

Download your `external_gateway_cert.pem` and `external_gateway_key.pem` from the Setup Instructions section of [bloXroute Portal Account Details](https://portal.bloxroute.com/details).

For the list of available endpoints, see [bloXroute Regional Providers](https://docs.bloxroute.com/solana/trader-api/introduction/regions).

Important:
- `client` can (and should) be reused for multiple transaction submissions. The connection is established on `connect()` and kept alive according to the configured timeouts.
- `tx_bytes` should be raw signed transaction bytes, not base64-encoded.
- We recommend using datagrams for best performance if your connection is stable.

```rust
// client configuration
let config = TraderApiQuicClientConfig::new_from_pem_files(
	"bloXroute regional endpoint, e.g. ny.solana.dex.blxrbdn.com",
	"/path/to/external_gateway_cert.pem",
	"/path/to/external_gateway_key.pem",
)?;

// create client and connect
let client = TraderApiQuicClient::connect(config).await?;
```

### Transaction Submission

Datagram:
```rust
client.send_transaction_datagram(&tx_bytes)?;
```

Unidirectional stream:
```rust
client.send_transaction_uni(&tx_bytes).await?;
```

Bidirectional stream:
```rust
let signature = client.send_transaction_bi(&tx_bytes).await?;
let signature = std::str::from_utf8(&signature)?;
```

These submission methods can be used in any combination and order. Standard transaction submission rate limits still apply.

## Example Program

The repository includes a runnable example:

```bash
cargo run --example quic_submit_example -- \
  --mode uni \
  --endpoint ny.solana.dex.blxrbdn.com \
  --tx-file /tmp/txBase64.txt \
  --client-cert /path/to/external_gateway_cert.pem \
  --client-key /path/to/external_gateway_key.pem
```

Available modes:

- `uni`
- `bi`
- `datagram`

For `bi`, the example prints the server response as UTF-8 text.
