fn main() {
    tracing_subscriber::fmt::init();
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        "nixfleet CLI skeleton"
    );
}
