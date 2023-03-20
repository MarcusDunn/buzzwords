use std::net::{AddrParseError, IpAddr};
use std::num::ParseIntError;
use std::time::Duration;
use amiquip::Channel;
use axum::body::Body;
use axum::extract::{FromRef, Path};
use axum::http::{Request, StatusCode};
use axum::Json;
use axum::response::Response;
use axum::routing::{get, post};
use serde::{Deserialize, Serialize};
use tower_http::metrics::in_flight_requests::InFlightRequestsLayer;
use tracing::{debug, info, Span, trace};

#[derive(Debug, Deserialize, Clone)]
struct NewUser {
    name: String,
    username: String,
    email: String,
    password: String,
}

#[derive(Serialize, Clone, Debug)]
struct Date; // todo

#[derive(Debug, Serialize, Clone)]
struct User {
    name: String,
    username: String,
    email: String,
    date_of_creation: Date,
    friends: Vec<String>,
}

#[derive(Debug, Deserialize, Clone)]
struct NewPost {
    title: String,
    content: String,
    author: String,
}

#[derive(Debug, Serialize, Clone)]
struct Post {
    id: u32,
    title: String,
    content: String,
    author: String,
    date_of_creation: Date,
    likes: Vec<String>,
    comments: Vec<u32>,
}

#[derive(Debug, Deserialize, Clone)]
struct NewComment {
    content: String,
    author: String,
}

#[derive(Debug, Serialize, Clone)]
struct Comment {
    id: u32,
    content: String,
    author: String,
    date_of_creation: Date,
    number_of_likes: u32,
}

#[derive(Debug, Serialize, Clone)]
enum Notification {
    NewPost {
        id: u32,
    },
    NewComment {
        id: u32,
    },
    PostLike {
        id: u32,
        username: String,
    },
    CommentLike {
        id: u32,
        username: String,
    },
}

#[derive(Debug)]
struct Application {
    redis: redis::Client,
    rabbit: amiquip::Connection,
}

impl FromRef<Application> for redis::Client {
    fn from_ref(input: &Application) -> Self {
        input.redis.clone()
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    trace!("Loading .env file");
    dotenvy::dotenv()?;

    let server_addr = std::net::SocketAddr::new(get_ip()?, get_port()?);

    let (in_flight_requests_layer, counter) = InFlightRequestsLayer::pair();

    tower_http::

    tokio::spawn(counter.run_emitter(Duration::from_secs(5), |count| async move {
        debug!(count, "requests in flight");
    }));

    let router = axum::Router::new()
        .route("/user/new", post(post_new_user))
        .route("/user/:username", get(get_user))
        .route("/user/:username/notifications", get(get_user_notification))
        .route("/post/new", post(post_new_post))
        .route("/post/:post_id/like", post(post_post_like))
        .route("/post/:post_id/comment", get(get_post_comment))
        .route("/post/:post_id/comment/new", post(post_new_comment))
        .route("/post/:post_id/comment/:comment_id/like", post(post_comment_like))
        .layer(in_flight_requests_layer)
        .layer(tower_http::trace::TraceLayer::new_for_http()
            .on_request(|_: &Request<_>, _: &Span| {
                debug!("received request")
            })
            .on_response(|resp: &Response<_>, duration: Duration, span: &Span| {
                debug!(status=?resp.status(), micros=duration.as_micros(), "finished processing request")
            })
        );

    let server = axum::Server::bind(&server_addr)
        .serve(router.into_make_service());

    info!("Server started on {}", server_addr);

    server.await?;

    Ok(())
}

#[axum::debug_handler]
#[tracing::instrument]
async fn post_new_user(Json(new_user): Json<NewUser>) -> Result<Json<User>, (StatusCode, String)> {
    Err((StatusCode::NOT_IMPLEMENTED, String::from("POST /user/new is not implemented")))
}

#[axum::debug_handler]
#[tracing::instrument]
async fn get_user(Path(username): Path<String>) -> Result<Json<User>, (StatusCode, String)> {
    Err((StatusCode::NOT_IMPLEMENTED, String::from("GET /user/:username is not implemented")))
}

#[axum::debug_handler]
#[tracing::instrument]
async fn post_new_post(Json(new_post): Json<NewPost>) -> Result<Json<Post>, (StatusCode, String)> {
    Err((StatusCode::NOT_IMPLEMENTED, String::from("POST /post/new is not implemented")))
}

#[axum::debug_handler]
#[tracing::instrument]
async fn get_post_comment(Path(post_id): Path<u32>) -> Result<Json<Vec<Comment>>, (StatusCode, String)> {
    Err((StatusCode::NOT_IMPLEMENTED, String::from("GET /post/:post_id/comment is not implemented")))
}

#[axum::debug_handler]
#[tracing::instrument]
async fn post_new_comment(Path(post_id): Path<u32>, Json(new_comment): Json<NewComment>) -> Result<Json<Comment>, (StatusCode, String)> {
    Err((StatusCode::NOT_IMPLEMENTED, String::from("POST /post/:post_id/comment/new is not implemented")))
}

#[axum::debug_handler]
#[tracing::instrument]
async fn get_user_notification(Path(username): Path<String>) -> Result<Json<Vec<Notification>>, (StatusCode, String)> {
    Err((StatusCode::NOT_IMPLEMENTED, String::from("GET /user/:username/notification is not implemented")))
}

#[axum::debug_handler]
#[tracing::instrument]
async fn post_post_like(Path(post_id): Path<u32>) -> Result<(), (StatusCode, String)> {
    Err((StatusCode::NOT_IMPLEMENTED, String::from("POST /post/:post_id/like is not implemented")))
}

#[axum::debug_handler]
#[tracing::instrument]
async fn post_comment_like(Path((post_id, comment_id)): Path<(u32, u32)>) -> Result<(), (StatusCode, String)> {
    Err((StatusCode::NOT_IMPLEMENTED, String::from("POST /post/:post_id/comment/:comment_id/like is not implemented")))
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
