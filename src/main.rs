use actix_web::middleware::Logger;
use actix_web::web::Data;
use actix_web::{web, App, HttpServer};
use clap::Parser;

use handlers::{root, AegisState};
use std::sync::Arc;
use tracing::level_filters::LevelFilter;
use tracing_subscriber::fmt::format::FmtSpan;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

use config::AegisConfig;
use tokio::sync::Mutex;
use tokio::time;

mod clients;
mod config;
mod handlers;
mod rules;

const DEFAULT_CONFIG_PATH: &str = "config.yaml";
const DEFAULT_LOG_ENV_FILTER: &str = "info,actix_server=error";

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    #[arg(short, long, default_value = DEFAULT_CONFIG_PATH)]
    config_file: String,
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    // Parse CLI args
    let args = Args::parse();

    // Fetch and validate config
    let config = fetch_config(&args);

    // Initialize logger
    init_logger(config.server.log_level.into_level_filter());

    // Init Redis client
    let redis_client: Option<clients::redis::RedisClient> =
        init_redis_client(config.redis.enabled, &config.redis.url).await;

    // Init http client
    let http_client = reqwest::Client::new();

    let listen_address = config.server.address.clone();
    let listen_port = config.server.port;

    // Init AegisState
    let state: AegisState = AegisState {
        config: Arc::new(Mutex::new(config)),
        redis_client,
        http_client,
    };

    // Watch config file for changes every 5 seconds
    tokio::spawn(config::watch_config(args.config_file, state.config.clone()));

    // Start Aegis server
    tracing::info!(
        "🚀 Aegis listening on address {}:{}",
        listen_address,
        listen_port
    );

    HttpServer::new(move || {
        App::new()
            .wrap(Logger::new("%a %r %s %b %{Referer}i %{User-Agent}i %D"))
            .app_data(Data::new(state.clone()))
            .default_service(web::to(root))
    })
    .bind((listen_address, listen_port))?
    .client_request_timeout(time::Duration::from_secs(10))
    .run()
    .await
}

fn init_logger(log_level: LevelFilter) {
    // Init logger
    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_span_events(FmtSpan::NONE)
        .with_target(true);

    let filter = EnvFilter::new(DEFAULT_LOG_ENV_FILTER);

    tracing_subscriber::registry()
        .with(log_level)
        .with(filter)
        .with(fmt_layer)
        .init();
}

fn fetch_config(args: &Args) -> AegisConfig {
    // Fetch config
    let config: AegisConfig = AegisConfig::from_file(&args.config_file).unwrap();
    config.validate().unwrap();

    config
}

async fn init_redis_client(
    redis_enabled: bool,
    redis_url: &String,
) -> Option<clients::redis::RedisClient> {
    if redis_enabled {
        let redis_client = Some(clients::redis::RedisClient::new(redis_url).await.unwrap());
        tracing::info!("🔌 Connected to redis");

        redis_client
    } else {
        tracing::warn!("Redis is disabled");
        None
    }
}
