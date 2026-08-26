//! The signing service binary.
//!
//! ```bash
//! stellaragent-signer --config /etc/signer/config.toml
//! stellaragent-signer issue-token          # print a token and its hash
//! stellaragent-signer check --config …     # validate config and exit
//! ```

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use stellaragent_signer::audit::{AuditLog, FileSink, Sink, StdoutSink};
use stellaragent_signer::backend::local::LocalKeystore;
use stellaragent_signer::backend::{BackendRegistry, SigningBackend};
use stellaragent_signer::config::{AuditSink, Config};
use stellaragent_signer::http::{self, AppState};
use stellaragent_signer::ledger::RatchetingClock;
use stellaragent_signer::metrics::Metrics;
use stellaragent_signer::policy::RateLimitState;
use stellaragent_signer::sign::SignerService;

#[tokio::main]
async fn main() -> std::process::ExitCode {
    match run().await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            // Startup failures go to stderr in plain text: they happen before
            // the log pipeline is anyone's concern, and an operator reading a
            // crashloop wants a sentence, not JSON.
            eprintln!("stellaragent-signer: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

enum Command {
    Serve(PathBuf),
    Check(PathBuf),
    IssueToken,
    Help,
}

fn parse_args() -> Result<Command, String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let config_of = |args: &[String]| -> Result<PathBuf, String> {
        args.iter()
            .position(|arg| arg == "--config")
            .and_then(|index| args.get(index + 1))
            .map(PathBuf::from)
            .ok_or_else(|| "--config <path> is required".to_string())
    };

    match args.first().map(String::as_str) {
        None | Some("serve") => config_of(&args).map(Command::Serve),
        Some("check") => config_of(&args).map(Command::Check),
        Some("issue-token") => Ok(Command::IssueToken),
        Some("--help" | "-h" | "help") => Ok(Command::Help),
        Some(other) => Err(format!("unknown command `{other}`")),
    }
}

async fn run() -> Result<(), String> {
    match parse_args()? {
        Command::Help => {
            println!(
                "stellaragent-signer — remote signing service\n\n\
                 USAGE:\n  \
                 stellaragent-signer [serve] --config <path>   Serve the protocol\n  \
                 stellaragent-signer check --config <path>     Validate configuration and exit\n  \
                 stellaragent-signer issue-token               Print a new token and its hash\n"
            );
            Ok(())
        }
        Command::IssueToken => {
            let token = stellaragent_signer::auth::generate_token();
            let hash = stellaragent_signer::auth::token_hash(&token);
            // The token goes to stdout once and is never stored. The hash is
            // what the operator pastes into the config.
            println!("token (give this to the agent, it is not recoverable):\n  {token}\n");
            println!("token_sha256 (put this in the config):\n  {hash}");
            Ok(())
        }
        Command::Check(path) => {
            let config = Config::load(&path).map_err(|error| error.to_string())?;
            let warnings = config.validate().map_err(|error| error.to_string())?;
            for warning in &warnings {
                println!("warning: {warning}");
            }
            println!(
                "configuration is valid: {} identities, {} policies, backends {:?}",
                config.registry.len(),
                config.policy.len(),
                config.backend_ids()
            );
            Ok(())
        }
        Command::Serve(path) => serve(path).await,
    }
}

async fn serve(config_path: PathBuf) -> Result<(), String> {
    init_tracing();

    let config = Config::load(&config_path).map_err(|error| error.to_string())?;
    let warnings = config.validate().map_err(|error| error.to_string())?;
    for warning in &warnings {
        tracing::warn!("{warning}");
    }

    // ── Backends ─────────────────────────────────────────────────────────────
    let mut backends: Vec<Box<dyn SigningBackend>> = Vec::new();
    if let Some(local) = &config.backend.local {
        let keystore = LocalKeystore::load(&local.path, local.acknowledge_insecure)
            .map_err(|error| error.to_string())?;
        backends.push(Box::new(keystore));
    }
    #[cfg(feature = "aws-kms")]
    if let Some(aws) = &config.backend.aws_kms {
        // The behaviour version is pinned rather than tracked: it governs
        // credential resolution, retries and timeouts, and an SDK upgrade
        // silently changing any of those for the process that signs payments
        // is not a surprise worth having.
        let mut loader = aws_config::defaults(aws_config::BehaviorVersion::v2026_01_12());
        if let Some(region) = &aws.region {
            loader = loader.region(aws_config::Region::new(region.clone()));
        }
        let client = aws_sdk_kms::Client::new(&loader.load().await);
        backends.push(Box::new(
            stellaragent_signer::backend::aws_kms::AwsKmsBackend::new(client),
        ));
    }
    #[cfg(not(feature = "aws-kms"))]
    if config.backend.aws_kms.is_some() {
        return Err("this binary was built without the `aws-kms` feature".into());
    }

    let backends = BackendRegistry::new(backends);

    // ── Everything else ──────────────────────────────────────────────────────
    let registry = config.key_registry();
    let policies = config.policy_set().map_err(|error| error.to_string())?;
    let tokens = config.token_store().map_err(|error| error.to_string())?;

    let sink: Box<dyn Sink> = match config.audit.sink {
        AuditSink::Stdout => Box::new(StdoutSink),
        AuditSink::File => {
            let path = config
                .audit
                .path
                .as_ref()
                .ok_or("[audit] sink = \"file\" requires a path")?;
            Box::new(FileSink::open(path).map_err(|error| {
                format!(
                    "could not open the audit log at {}: {error}",
                    path.display()
                )
            })?)
        }
    };

    let metrics = Arc::new(Metrics::new());
    let service = Arc::new(SignerService {
        backends,
        registry,
        policies,
        tokens,
        audit: AuditLog::new(sink),
        metrics: Arc::clone(&metrics),
        rate_limits: RateLimitState::new(),
        clock: Arc::new(RatchetingClock::new(
            config.ledger.initial,
            config.ledger.max_advance,
        )),
    });

    // Resolve every key once, so a wrong ARN or a non-Ed25519 key spec is a
    // startup failure rather than a surprise on the first payment.
    for key in service.registry.keys() {
        let backend = service
            .backends
            .get(key)
            .map_err(|error| format!("key {key}: {error}"))?;
        let public = backend
            .public_key(key)
            .await
            .map_err(|error| format!("key {key}: {error}"))?;
        tracing::info!(
            key = %key,
            address = %stellaragent_signer::stellar::address_from_public_key(&public),
            "resolved signing key"
        );
    }

    for (key, subjects) in service.registry.shared_keys() {
        tracing::warn!(
            key = %key,
            subjects = ?subjects.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
            "several identities share one key: which agent spent what is answerable only from \
             the audit log, not from the chain"
        );
    }

    // ── Listeners ────────────────────────────────────────────────────────────
    let app = http::router(
        AppState {
            service: Arc::clone(&service),
        },
        config.server.max_body_bytes,
        Duration::from_secs(config.server.request_timeout_seconds),
    )
    .fallback(http::not_found);

    let listener = tokio::net::TcpListener::bind(&config.server.bind)
        .await
        .map_err(|error| format!("could not bind {}: {error}", config.server.bind))?;

    tracing::info!(
        bind = %config.server.bind,
        identities = service.registry.len(),
        "signing service ready"
    );

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(|error| format!("server error: {error}"))?;

    // The head hash is what makes the chain evidence rather than a log. An
    // operator should anchor it somewhere this process cannot reach.
    tracing::info!(
        audit_head = %service.audit.head(),
        records = service.audit.len(),
        "shut down; anchor this audit head hash externally"
    );

    Ok(())
}

fn init_tracing() {
    use tracing_subscriber::{fmt, prelude::*, EnvFilter};

    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("stellaragent_signer=info,tower_http=warn"));

    // JSON to stderr: the audit log owns stdout, and mixing the two would make
    // the audit stream unparseable.
    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().json().with_writer(std::io::stderr))
        .init();
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut signal) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            signal.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }

    tracing::info!("shutdown signal received; draining");
}
