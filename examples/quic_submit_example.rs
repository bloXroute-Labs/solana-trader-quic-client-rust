use base64::Engine;
use solana_trader_quic_client_rust::{TraderApiQuicClient, TraderApiQuicClientConfig};
use std::env;
use std::fs;
use std::time::Duration;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse()?;
    let tx_bytes = load_transaction(&args)?;

    let config = TraderApiQuicClientConfig::new_from_pem_files(
        &args.endpoint,
        &args.client_cert,
        &args.client_key,
    )?;

    let client = TraderApiQuicClient::connect(config).await?;
    match args.mode {
        SubmissionMode::Uni => {
            client.send_transaction_uni(&tx_bytes).await?;
        }
        SubmissionMode::Bi => {
            let signature = client.send_transaction_bi(&tx_bytes).await?;
            let signature = std::str::from_utf8(&signature)?;
            println!("received response: {}", signature);
        }
        SubmissionMode::Datagram => {
            client.send_transaction_datagram(&tx_bytes)?;
        }
    }

    match tokio::time::timeout(Duration::from_secs(1), client.wait_for_close()).await {
        Ok(error) => {
            return Err(format!("server closed QUIC connection after submit: {error}").into());
        }
        Err(_) => {}
    }

    println!(
        "submitted {} transaction bytes to {} over QUIC {}",
        tx_bytes.len(),
        args.endpoint,
        args.mode.name()
    );

    Ok(())
}

struct Args {
    endpoint: String,
    mode: SubmissionMode,
    tx_base64: Option<String>,
    tx_file: Option<String>,
    client_cert: String,
    client_key: String,
}

#[derive(Clone, Copy)]
enum SubmissionMode {
    Uni,
    Bi,
    Datagram,
}

impl SubmissionMode {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "uni" => Ok(Self::Uni),
            "bi" => Ok(Self::Bi),
            "datagram" => Ok(Self::Datagram),
            _ => Err(format!("invalid --mode value: {value}\n\n{}", usage())),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Uni => "uni stream",
            Self::Bi => "bi stream",
            Self::Datagram => "datagram",
        }
    }
}

impl Args {
    fn parse() -> Result<Self, String> {
        let mut endpoint = "127.0.0.1:9000".to_string();
        let mut mode = SubmissionMode::Uni;
        let mut tx_base64 = None;
        let mut tx_file = None;
        let mut client_cert = None;
        let mut client_key = None;

        let mut args = env::args().skip(1);
        while let Some(flag) = args.next() {
            match flag.as_str() {
                "--endpoint" => endpoint = parse_value(&mut args, "--endpoint")?,
                "--mode" => mode = SubmissionMode::parse(&parse_value(&mut args, "--mode")?)?,
                "--tx-base64" => tx_base64 = Some(parse_value(&mut args, "--tx-base64")?),
                "--tx-file" => tx_file = Some(parse_value(&mut args, "--tx-file")?),
                "--client-cert" => client_cert = Some(parse_value(&mut args, "--client-cert")?),
                "--client-key" => client_key = Some(parse_value(&mut args, "--client-key")?),
                "--help" | "-h" => return Err(usage()),
                other => return Err(format!("unknown argument: {other}\n\n{}", usage())),
            }
        }

        let client_cert = client_cert.ok_or_else(usage)?;
        let client_key = client_key.ok_or_else(usage)?;
        if tx_base64.is_some() == tx_file.is_some() {
            return Err(format!(
                "exactly one of --tx-base64 or --tx-file is required\n\n{}",
                usage()
            ));
        }

        Ok(Self {
            endpoint: endpoint,
            mode: mode,
            tx_base64: tx_base64,
            tx_file: tx_file,
            client_cert: client_cert,
            client_key: client_key,
        })
    }
}

fn load_transaction(args: &Args) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let encoded = match (&args.tx_base64, &args.tx_file) {
        (Some(value), None) => value.clone(),
        (None, Some(path)) => fs::read_to_string(path)?,
        _ => unreachable!("validated by Args::parse"),
    };

    let trimmed = encoded.trim();
    if trimmed.is_empty() {
        return Err("transaction payload is empty".into());
    }

    let tx_bytes = base64::engine::general_purpose::STANDARD.decode(trimmed)?;
    if tx_bytes.is_empty() {
        return Err("transaction payload is empty".into());
    }

    Ok(tx_bytes)
}

fn parse_value(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
    args.next()
        .ok_or_else(|| format!("missing value for {flag}\n\n{}", usage()))
}

fn usage() -> String {
    "\
Usage:
  cargo run --example quic_submit_example -- \
    --endpoint localhost:9000 \
    [--mode uni|bi|datagram] \
    --tx-base64 <base64 signed tx> \
    --client-cert client.pem \
    --client-key client.key

Notes:
  Exactly one of --tx-base64 or --tx-file is required.
  --mode defaults to uni.
  --endpoint accepts a hostname or IP, with port 443 used by default.
  --endpoint also accepts explicit host:port or ip:port.
  --tx-file should contain base64-encoded signed transaction bytes.
"
    .to_string()
}
