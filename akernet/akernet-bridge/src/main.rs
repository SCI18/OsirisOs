use axum::{
    routing::get,
    Router,
    Json,
};
use serde::Serialize;

#[derive(Serialize)]
struct Health {
    status: String,
    service: String,
    version: String,
}

#[tokio::main]
async fn main() {
    // Initialize tracing
    tracing_subscriber::fmt::init();

    let app = Router::new()
        .route("/health", get(health_check))
        .route("/", get(root));

    let listener = tokio::net::TcpListener::bind("0.0.0.0:7474")
        .await
        .expect("Failed to bind to port 7474");

    tracing::info!("AkerNet Bridge v0.1.0 listening on 0.0.0.0:7474");

    axum::serve(listener, app)
        .await
        .expect("Server failed");
}

async fn root() -> &'static str {
    "AkerNet Bridge — The Guardian is awake."
}

async fn health_check() -> Json<Health> {
    Json(Health {
        status: "ok".to_string(),
        service: "akernet-bridge".to_string(),
        version: "0.1.0".to_string(),
    })
}
