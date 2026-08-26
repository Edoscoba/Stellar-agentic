//! `signer-conformance` — verify an implementation of the remote signing
//! protocol.
//!
//! ```bash
//! # Grade a server:
//! signer-conformance server --url https://signer.internal \
//!     --token "$SIGNER_TOKEN" --network "Test SDF Network ; September 2015"
//!
//! # Grade a client: start the reference server, point the client at it,
//! # then stop it with Ctrl-C to print the report.
//! signer-conformance client --port 8099
//! ```

use std::process::ExitCode;

use stellaragent_signer_conformance::{client, server};

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let result = match args.first().map(String::as_str) {
        Some("server") => run_server(&args).await,
        Some("client") => run_client(&args).await,
        Some("--help" | "-h" | "help") | None => {
            print_usage();
            return ExitCode::SUCCESS;
        }
        Some(other) => Err(format!("unknown mode `{other}`")),
    };

    match result {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("signer-conformance: {error}");
            ExitCode::FAILURE
        }
    }
}

fn print_usage() {
    println!(
        "signer-conformance — verify an implementation of the StellarAgent remote signing protocol\n\n\
         USAGE:\n  \
         signer-conformance server --url <url> --token <token> [--network <passphrase>]\n\
         \x20                        [--invalid-token <token>] [--envelope <base64>]\n  \
         signer-conformance client [--port <port>]\n\n\
         SERVER MODE grades a running service against docs/signing.md.\n\
         \x20 --envelope is an unsigned transaction the service's policy permits. Without it the\n\
         \x20 signing checks are skipped rather than guessed at, because an envelope policy\n\
         \x20 refuses would make a conforming service look broken.\n\n\
         CLIENT MODE stands up a reference server and grades the requests a client sends it.\n\
         \x20 Point the client under test at the printed URL, exercise every method, then stop\n\
         \x20 this process with Ctrl-C to print the report.\n"
    );
}

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|arg| arg == name)
        .and_then(|index| args.get(index + 1))
        .cloned()
}

async fn run_server(args: &[String]) -> Result<bool, String> {
    let url = flag(args, "--url").ok_or("--url is required")?;
    let token = flag(args, "--token").ok_or("--token is required")?;
    let network =
        flag(args, "--network").unwrap_or_else(|| "Test SDF Network ; September 2015".to_string());

    let target = server::Target {
        url,
        token,
        // A token the service must reject. Random by default so it cannot
        // accidentally be one the service knows.
        invalid_token: flag(args, "--invalid-token")
            .unwrap_or_else(|| "conformance-invalid-token-000000000000".to_string()),
        signable_envelope: flag(args, "--envelope"),
        network_passphrase: network,
    };

    println!("Grading {} against docs/signing.md\n", target.url);
    let report = server::run(&target).await;
    print!("{report}");
    Ok(report.is_conformant())
}

async fn run_client(args: &[String]) -> Result<bool, String> {
    let port: u16 = flag(args, "--port")
        .unwrap_or_else(|| "0".into())
        .parse()
        .map_err(|_| "--port must be a number".to_string())?;

    let server = client::ReferenceServer::start(port)
        .await
        .map_err(|error| format!("could not bind: {error}"))?;

    println!("Reference server listening on {}", server.url());
    println!("  token:            {}", client::EXPECTED_TOKEN);
    println!("  expectedPublicKey: {}", client::reference_address());
    println!("\nPoint the client under test at that URL, exercise getPublicKey,");
    println!("signTransaction and signAuthEntry, then press Ctrl-C for the report.\n");

    let _ = tokio::signal::ctrl_c().await;

    let report = server.report();
    println!("\n{report}");
    Ok(report.is_conformant())
}
