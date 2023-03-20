use std::fmt::{Debug, Formatter};
use axum::extract::{Path, State};
use axum::http::{Request, StatusCode};
use axum::response::Response;
use axum::routing::{get, post};
use axum::Json;
use mongodb::options::{ClientOptions, IndexOptions};
use serde::{Deserialize, Serialize};
use std::net::{AddrParseError, IpAddr};
use std::num::ParseIntError;
use std::time::Duration;
use anyhow::anyhow;
use deadpool_redis::Runtime;
use mongodb::{Database, IndexModel};
use mongodb::results::CreateIndexResult;
use redis::{AsyncCommands, FromRedisValue, RedisResult, Value};
use time::{Date, OffsetDateTime};
use tracing::{debug, info, trace, Span, error, warn};

#[derive(Debug, Deserialize, Clone)]
struct NewUser {
    name: String,
    username: String,
    email: String,
    password: String,
}

#[derive(Debug, Serialize, Clone, Deserialize)]
struct User {
    name: String,
    username: String,
    email: String,
    date_of_creation: Date,
    friends: Vec<String>,
}

impl FromRedisValue for User {
    fn from_redis_value(v: &Value) -> RedisResult<Self> {
        if let Value::Data(ref bytes) = v {
            Ok(serde_json::from_slice(bytes.as_slice()).map_err(|err| {
                error!("failed to parse redis bytes into a User: {err}");
                (redis::ErrorKind::TypeError, "failed to parse redis bytes into a User", format!("{err}"))
            })?)
        } else {
            error!("incorrect type when turning a redis value into a User: {v:?}");
            Err((redis::ErrorKind::TypeError, "incorrect type to turn into User").into())
        }
    }
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
    NewPost { id: u32 },
    NewComment { id: u32 },
    PostLike { id: u32, username: String },
    CommentLike { id: u32, username: String },
}

#[derive(axum::extract::FromRef, Clone)]
struct Application {
    redis: deadpool_redis::Pool,
    rabbit: deadpool_lapin::Pool,
    mongo: mongodb::Client,
}

impl Debug for Application {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Application")
            .field("redis", &"deadpool_redis::Pool")
            .field("rabbit", &self.rabbit)
            .field("mongo", &self.mongo)
            .finish()
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    trace!("Loading .env file");
    if let Err(err) = dotenvy::dotenv() {
        warn!("error loading .env file: {}", err)
    }

    let server_addr = std::net::SocketAddr::new(get_ip()?, get_port()?);

    let redis = get_redis()?;
    let mongo = get_mongodb().await?;
    index_mongo(&mongo).await?;

    let rabbit = get_rabbitmq().await?;

    let application = Application {
        redis,
        rabbit,
        mongo,
    };

    let router = axum::Router::new()
        .route("/healthcheck", get(|| async { "OK" }))
        .route("/user/new", post(post_new_user))
        .route("/user/:username", get(get_user))
        .route("/user/:username/notifications", get(get_user_notification))
        .route("/post/new", post(post_new_post))
        .route("/post/:post_id/like", post(post_post_like))
        .route("/post/:post_id/comment", get(get_post_comment))
        .route("/post/:post_id/comment/new", post(post_new_comment))
        .route("/post/:post_id/comment/:comment_id/like", post(post_comment_like))
        .layer(tower_http::trace::TraceLayer::new_for_http()
            .on_request(|_: &Request<_>, _: &Span| {
                debug!("received request")
            })
            .on_response(|resp: &Response<_>, duration: Duration, _: &Span| {
                debug!(status=?resp.status(), micros=duration.as_micros(), "finished processing request")
            })
        ).with_state(application);

    let server = axum::Server::bind(&server_addr)
        .serve(router.into_make_service())
        .with_graceful_shutdown(async {
            tokio::signal::ctrl_c().await.expect("failed to listen for CTRL+C");
            info!("Shutting down");
        });

    info!("Server started on {}", server_addr);

    server.await?;

    Ok(())
}

async fn index_mongo(mongo: &mongodb::Client) -> anyhow::Result<()> {
    let database = mongo
        .default_database()
        .ok_or_else(|| anyhow!("no default database"))?;

    async fn index_users(database: &Database) -> mongodb::error::Result<CreateIndexResult> {
        trace!("creating user index on username");
        database
            .collection::<User>("user")
            .create_index(
                IndexModel::builder()
                    .keys(mongodb::bson::doc! { "username": 1 })
                    .options(IndexOptions::builder()
                        .unique(true)
                        .build()
                    )
                    .build(),
                None,
            ).await
    }

    tokio::try_join!(index_users(&database))?;

    Ok(())
}

#[axum::debug_handler(state = Application)]
#[tracing::instrument(skip(mongo))]
async fn post_new_user(
    State(mongo): State<mongodb::Client>,
    Json(new_user): Json<NewUser>,
) -> Result<Json<User>, (StatusCode, String)> {
    let db = mongo.default_database().ok_or_else(|| {
        error!("No default database specified");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            String::from("No default database specified"),
        )
    })?;
    let user = User {
        name: new_user.name,
        username: new_user.username,
        email: new_user.email,
        date_of_creation: OffsetDateTime::now_utc().date(),
        friends: Vec::new(),
    };
    db.collection("users")
        .insert_one(
            mongodb::bson::to_document(&user).map_err(|err| {
                error!("failed to insert user: {err}");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("failed to insert user: {err}"),
                )
            })?,
            None,
        )
        .await
        .map_err(|err| {
            error!("failed to insert user: {err}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to insert user: {err}"),
            )
        })?;
    trace!("Inserted user: {:?}", user);
    Ok(Json(user))
}

#[axum::debug_handler(state = Application)]
#[tracing::instrument(skip(mongo, redis))]
async fn get_user(
    State(mongo): State<mongodb::Client>,
    State(redis): State<deadpool_redis::Pool>,
    Path(username): Path<String>,
) -> Result<Json<User>, (StatusCode, String)> {
    let mut redis_conn = redis.get().await.map_err(|err| {
        error!("failed to obtain redis connection: {err}");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to obtain redis connection: {err}"),
        )
    })?;

    if let Some(user) = redis_conn.get(&username).await.map_err(|err| {
        error!("failed to fetch user from redis: {err}");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to fetch user from redis: {err}"),
        )
    })? {
        trace!("got user from redis cache: {user:?}");
        return Ok(Json(user));
    } else {
        trace!("user not found in redis cache");
    }

    let db = mongo.default_database().ok_or_else(|| {
        error!("No default database specified");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            String::from("No default database specified"),
        )
    })?;

    let user: User = db
        .collection("users")
        .find_one(
            mongodb::bson::doc! { "username": &username },
            None,
        )
        .await
        .map_err(|err| {
            error!("failed to find user: {err}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to find user: {err}"),
            )
        })?
        .ok_or_else(|| {
            error!("user {username} not found");
            (
                StatusCode::BAD_REQUEST,
                format!("user {username} not found"),
            )
        })?;
    trace!("found user: {:?}", &user);

    let user_json = serde_json::to_string(&user).map_err(|err| {
        error!("failed to serialize user: {err}");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to serialize user: {err}"),
        )
    })?;

    redis_conn.set(username, user_json).await.map_err(|err| {
        error!("failed to insert user into redis cache: {err}");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to insert user into redis cache: {err}"),
        )
    })?;

    trace!("inserted user into redis cache");

    Ok(Json(user))
}

#[axum::debug_handler]
#[tracing::instrument]
async fn post_new_post(Json(new_post): Json<NewPost>) -> Result<Json<Post>, (StatusCode, String)> {
    Err((
        StatusCode::NOT_IMPLEMENTED,
        String::from("POST /post/new is not implemented"),
    ))
}

#[axum::debug_handler]
#[tracing::instrument]
async fn get_post_comment(
    Path(post_id): Path<u32>,
) -> Result<Json<Vec<Comment>>, (StatusCode, String)> {
    Err((
        StatusCode::NOT_IMPLEMENTED,
        String::from("GET /post/:post_id/comment is not implemented"),
    ))
}

#[axum::debug_handler]
#[tracing::instrument]
async fn post_new_comment(
    Path(post_id): Path<u32>,
    Json(new_comment): Json<NewComment>,
) -> Result<Json<Comment>, (StatusCode, String)> {
    Err((
        StatusCode::NOT_IMPLEMENTED,
        String::from("POST /post/:post_id/comment/new is not implemented"),
    ))
}

#[axum::debug_handler]
#[tracing::instrument]
async fn get_user_notification(
    Path(username): Path<String>,
) -> Result<Json<Vec<Notification>>, (StatusCode, String)> {
    Err((
        StatusCode::NOT_IMPLEMENTED,
        String::from("GET /user/:username/notification is not implemented"),
    ))
}

#[axum::debug_handler]
#[tracing::instrument]
async fn post_post_like(Path(post_id): Path<u32>) -> Result<(), (StatusCode, String)> {
    Err((
        StatusCode::NOT_IMPLEMENTED,
        String::from("POST /post/:post_id/like is not implemented"),
    ))
}

#[axum::debug_handler]
#[tracing::instrument]
async fn post_comment_like(
    Path((post_id, comment_id)): Path<(u32, u32)>,
) -> Result<(), (StatusCode, String)> {
    Err((
        StatusCode::NOT_IMPLEMENTED,
        String::from("POST /post/:post_id/comment/:comment_id/like is not implemented"),
    ))
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

fn get_redis() -> anyhow::Result<deadpool_redis::Pool> {
    trace!("Reading REDIS_URL");
    let redis_url = std::env::var("REDIS_URL")?;
    let client = deadpool_redis::Config::from_url(redis_url).create_pool(Some(Runtime::Tokio1))?;
    trace!("Redis client created");
    Ok(client)
}

async fn get_mongodb() -> anyhow::Result<mongodb::Client> {
    trace!("Reading MONGO_URL");
    let mongo_url = std::env::var("MONGO_URL")?.parse()?;
    let options = ClientOptions::parse_connection_string(mongo_url).await?;
    trace!(?options, "Creating mongo client");
    let client = mongodb::Client::with_options(options)?;
    trace!("Mongo client created");
    Ok(client)
}

async fn get_rabbitmq() -> anyhow::Result<deadpool_lapin::Pool> {
    use deadpool_lapin::Config;
    trace!("Reading RABBITMQ_URL");
    let rabbitmq_url = std::env::var("RABBITMQ_URL")?;
    let config = Config { url: Some(rabbitmq_url), ..Config::default() };
    let pool = config.create_pool(Some(Runtime::Tokio1))?;
    trace!("RabbitMQ channel created");
    Ok(pool)
}
