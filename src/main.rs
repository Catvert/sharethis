use sharethis::{App, Config};

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt::init();

    let config = Config::from_env();
    let port = config.port;

    let app = match App::init(config).await {
        Ok(app) => app,
        Err(e) => fatal(e),
    };
    let listener = match tokio::net::TcpListener::bind(("0.0.0.0", port)).await {
        Ok(l) => l,
        Err(e) => fatal(format!("bind port {port}: {e}")),
    };
    tracing::info!("Server running on http://localhost:{port}");
    if let Err(e) = app.serve(listener, shutdown_signal()).await {
        fatal(format!("server: {e}"));
    }
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}

fn fatal(msg: impl std::fmt::Display) -> ! {
    tracing::error!("{msg}");
    std::process::exit(1);
}
