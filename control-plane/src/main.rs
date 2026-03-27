use clap::Parser;
use nixfleet_control_plane::{build_app, db, state};
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Parser)]
#[command(name = "nixfleet-control-plane", about = "NixFleet control plane server")]
struct Cli {
    /// Address to listen on
    #[arg(long, default_value = "0.0.0.0:8080", env = "NIXFLEET_CP_LISTEN")]
    listen: String,

    /// SQLite database path for persistent state
    #[arg(
        long,
        default_value = "/var/lib/nixfleet-cp/state.db",
        env = "NIXFLEET_CP_DB_PATH"
    )]
    db_path: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "nixfleet=info,tower_http=info".into()),
        )
        .init();

    let cli = Cli::parse();

    let fleet_state = Arc::new(RwLock::new(state::FleetState::new()));
    let db = Arc::new(db::Db::new(&cli.db_path)?);
    db.migrate()?;

    // Hydrate in-memory state from DB on startup
    state::hydrate_from_db(&fleet_state, &db).await?;

    let app = build_app(fleet_state, db);

    let listener = tokio::net::TcpListener::bind(&cli.listen).await?;
    tracing::info!("Control plane listening on {}", cli.listen);
    axum::serve(listener, app).await?;
    Ok(())
}
