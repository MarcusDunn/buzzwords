use axum::routing::{get, post};
use std::net::{AddrParseError, IpAddr};
use std::num::ParseIntError;
use std::time::Duration;
use tower_http::timeout::Timeout;
use tower_http::trace::{DefaultMakeSpan, DefaultOnResponse, TraceLayer};
use tower_http::{LatencyUnit, ServiceBuilderExt};
use tracing::{info, trace, warn};
use tracing_subscriber::fmt::format::FmtSpan;
use tracing_subscriber::EnvFilter;
use crate::comment::{get_post_comment, post_comment_like, post_new_comment};
use crate::follow::post_user_follow;
use crate::notification::get_user_notification;
use crate::post::{post_new_post, post_post_like};
use crate::user::{get_user, post_new_user};
use crate::redis::{get_redis};
use crate::mongo::{get_mongodb, index_mongo};

mod user;
mod post;
mod comment;
mod notification;
mod follow;
mod mongo;
mod redis;
mod ampq;

#[derive(axum::extract::FromRef, Clone)]
pub struct Application {
    redis: deadpool_redis::Pool,
    rabbit: deadpool_lapin::Pool,
    mongo: mongodb::Client,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_span_events(FmtSpan::NEW)
        .with_span_events(FmtSpan::CLOSE)
        .init();

    trace!("Loading .env file");
    if let Err(err) = dotenvy::dotenv() {
        warn!("error loading .env file: {}", err)
    }

    let server_addr = std::net::SocketAddr::new(get_ip()?, get_port()?);

    let redis = get_redis()?;
    let mongo = get_mongodb().await?;
    index_mongo(&mongo).await?;

    let rabbit = ampq::get_rabbitmq().await?;

    let application = Application {
        redis,
        rabbit,
        mongo,
    };

    let router = axum::Router::new()
        .route("/healthcheck", get(|| async { "OK" }))
        .route("/user/new", post(post_new_user))
        .route("/user/:username", get(get_user))
        .route("/user/:username/follow", post(post_user_follow))
        .route("/user/:username/notifications", get(get_user_notification))
        .route("/user/:username/post/new", post(post_new_post))
        .route(
            "/user/:username/post/:post_title/like",
            post(post_post_like),
        )
        .route(
            "/user/:username/post/:post_title/comment",
            get(get_post_comment),
        )
        .route(
            "/user/:username/post/:post_title/comment/new",
            post(post_new_comment),
        )
        .route(
            "/user/:username/post/:post_title/comment/:comment_title/like",
            post(post_comment_like),
        )
        .layer(tower::ServiceBuilder::new().propagate_x_request_id())
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(DefaultMakeSpan::new().include_headers(true))
                .on_response(DefaultOnResponse::default().latency_unit(LatencyUnit::Micros)),
        )
        .layer(
            tower::ServiceBuilder::new()
                .set_x_request_id(tower_http::request_id::MakeRequestUuid {}),
        )
        .layer(Timeout::<()>::layer(Duration::from_secs(2)))
        .with_state(application);

    let server = axum::Server::bind(&server_addr)
        .serve(router.into_make_service())
        .with_graceful_shutdown(async {
            tokio::signal::ctrl_c()
                .await
                .expect("failed to listen for CTRL+C");
            info!("Shutting down");
        });

    info!("Server started on {}", server_addr);

    server.await?;

    Ok(())
}

fn get_ip() -> Result<IpAddr, AddrParseError> {
    trace!("Reading SERVER_ADDR");
    let ip = std::env::var("SERVER_ADDR")
        .unwrap_or_else(|_| {
            trace!("SERVER_ADDR not found, using default value: 0.0.0.0");
            String::from("0.0.0.0")
        })
        .parse()?;
    trace!("Server IP: {}", ip);
    Ok(ip)
}

#[tracing::instrument]
fn get_port() -> Result<u16, ParseIntError> {
    trace!("Reading SERVER_PORT");
    let port = std::env::var("SERVER_PORT")
        .unwrap_or_else(|_| {
            trace!("SERVER_PORT not found, using default value: 3000");
            String::from("3000")
        })
        .parse()?;
    trace!("Server Port: {}", port);
    Ok(port)
}
