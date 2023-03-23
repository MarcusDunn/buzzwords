use axum::extract::{FromRef, FromRequestParts, Path};
use axum::http::StatusCode;
use std::fmt::Debug;

use anyhow::anyhow;
use axum::http::request::Parts;
use axum::routing::{get, post};
use axum::Json;
use deadpool_redis::{Connection, Runtime};
use lapin::options::{
    BasicAckOptions, BasicGetOptions, BasicPublishOptions, QueueDeclareOptions,
};
use lapin::types::FieldTable;
use lapin::BasicProperties;
use mongodb::bson::doc;
use mongodb::options::{ClientOptions, FindOneAndUpdateOptions, IndexOptions, ReturnDocument};
use mongodb::results::CreateIndexResult;
use mongodb::{bson, Database, IndexModel};
use redis::{AsyncCommands, FromRedisValue, RedisResult, Value};
use serde::{Deserialize, Serialize};
use std::net::{AddrParseError, IpAddr};
use std::num::ParseIntError;
use std::time::Duration;
use time::{Date, OffsetDateTime};
use tower_http::timeout::Timeout;
use tower_http::trace::{DefaultMakeSpan, DefaultOnResponse, TraceLayer};
use tower_http::{LatencyUnit, ServiceBuilderExt};
use tracing::{error, info, trace, warn};
use tracing_subscriber::fmt::format::FmtSpan;
use tracing_subscriber::EnvFilter;

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
    password: String,
    email: String,
    date_of_creation: Date,
    followers: Vec<String>,
}

impl FromRedisValue for User {
    fn from_redis_value(v: &Value) -> RedisResult<Self> {
        if let Value::Data(ref bytes) = v {
            Ok(serde_json::from_slice(bytes.as_slice()).map_err(|err| {
                error!("failed to parse redis bytes into a User: {err}");
                (
                    redis::ErrorKind::TypeError,
                    "failed to parse redis bytes into a User",
                    format!("{err}"),
                )
            })?)
        } else {
            error!("incorrect type when turning a redis value into a User: {v:?}");
            Err((
                redis::ErrorKind::TypeError,
                "incorrect type to turn into User",
            )
                .into())
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
struct NewPost {
    title: String,
    content: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct Post {
    title: String,
    content: String,
    author: String,
    date_of_creation: Date,
    likes: Vec<String>,
    comments: Vec<Comment>,
}

#[derive(Debug, Deserialize, Clone)]
struct NewComment {
    content: String,
    author: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct Comment {
    content: String,
    author: String,
    date_of_creation: Date,
    number_of_likes: u32,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
enum Notification {
    NewPost(Post),
    NewPostLike(NewPostLike)
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct NewPostLike {
    title: String,
    liker: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct Follow {
    follower: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct Like {
    liker: String,
}

#[derive(axum::extract::FromRef, Clone)]
struct Application {
    redis: deadpool_redis::Pool,
    rabbit: deadpool_lapin::Pool,
    mongo: mongodb::Client,
}

struct Mongo(Database);

#[axum::async_trait]
impl<S> FromRequestParts<S> for Mongo
where
    mongodb::Client: FromRef<S>,
    S: Sync,
{
    type Rejection = (StatusCode, String);

    #[tracing::instrument(name = "get mongo default database", skip_all)]
    async fn from_request_parts(_: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let client = mongodb::Client::from_ref(state);
        let db = client.default_database().ok_or_else(|| {
            error!("No default database specified");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                String::from("No default database specified"),
            )
        })?;
        Ok(Mongo(db))
    }
}

struct Redis(Connection);

#[axum::async_trait]
impl<S> FromRequestParts<S> for Redis
where
    deadpool_redis::Pool: FromRef<S>,
    S: Sync,
{
    type Rejection = (StatusCode, String);

    #[tracing::instrument(name = "get redis connection", skip_all)]
    async fn from_request_parts(_parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let redis = deadpool_redis::Pool::from_ref(state);
        trace!("redis pool status: {:?}", redis.status());
        let redis_conn = redis.get().await.map_err(|err| {
            error!("failed to obtain redis connection: {err}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to obtain redis connection: {err}"),
            )
        })?;
        Ok(Redis(redis_conn))
    }
}

struct RabbitMQ(deadpool_lapin::Connection);

#[axum::async_trait]
impl<S> FromRequestParts<S> for RabbitMQ
where
    deadpool_lapin::Pool: FromRef<S>,
    S: Sync,
{
    type Rejection = (StatusCode, String);

    #[tracing::instrument(name = "get rabbitmq connection", skip_all)]
    async fn from_request_parts(_parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let rabbit = deadpool_lapin::Pool::from_ref(state);
        trace!("rabbit pool status: {:?}", rabbit.status());
        let rabbit_conn = rabbit.get().await.map_err(|err| {
            error!("failed to obtain rabbitmq connection: {err}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to obtain rabbitmq connection: {err}"),
            )
        })?;
        Ok(RabbitMQ(rabbit_conn))
    }
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
            "/post/:post_id/comment/:comment_id/like",
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
                    .keys(doc! { "name": 1 })
                    .options(IndexOptions::builder().unique(true).build())
                    .build(),
                None,
            )
            .await
    }

    async fn index_posts(database: &Database) -> mongodb::error::Result<CreateIndexResult> {
        trace!("creating post index on title and username");
        database
            .collection::<Post>("post")
            .create_index(
                IndexModel::builder()
                    .keys(doc! { "title": 1, "username": 1 })
                    .options(IndexOptions::builder().unique(true).build())
                    .build(),
                None,
            )
            .await
    }

    tokio::try_join!(index_users(&database), index_posts(&database))?;

    Ok(())
}

#[axum::debug_handler(state = Application)]
#[tracing::instrument(skip(db, redis))]
async fn post_user_follow(
    Mongo(db): Mongo,
    Redis(mut redis): Redis,
    Path(username): Path<String>,
    Json(Follow { follower }): Json<Follow>,
) -> Result<(), (StatusCode, String)> {
    let user = db
        .collection::<User>("user")
        .find_one_and_update(
            doc! { "username": &username },
            doc! { "$push": { "followers": &follower } },
            FindOneAndUpdateOptions::builder()
                .return_document(ReturnDocument::After)
                .build(),
        )
        .await
        .map_err(|err| {
            error!("failed to follow: {err}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to follow: {err}"),
            )
        })?;

    let user_json_bytes = serde_json::to_vec(&user).map_err(|err| {
        error!("failed to serialize user: {err}");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to serialize user: {err}"),
        )
    })?;

    redis.set(&username, user_json_bytes).await.map_err(|err| {
        error!("failed to update redis: {err}");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed update redis: {err}"),
        )
    })?;

    trace!("{follower} followed user: {username}");

    Ok(())
}

#[axum::debug_handler(state = Application)]
#[tracing::instrument(skip(db))]
async fn post_new_user(
    Mongo(db): Mongo,
    Json(new_user): Json<NewUser>,
) -> Result<Json<User>, (StatusCode, String)> {
    let user = User {
        name: new_user.name,
        username: new_user.username,
        email: new_user.email,
        password: new_user.password,
        date_of_creation: OffsetDateTime::now_utc().date(),
        followers: Vec::new(),
    };
    db.collection("user")
        .insert_one(
            bson::to_document(&user).map_err(|err| {
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
#[tracing::instrument(skip(db, redis))]
async fn get_user(
    Mongo(db): Mongo,
    Redis(mut redis): Redis,
    Path(username): Path<String>,
) -> Result<Json<User>, (StatusCode, String)> {
    if let Some(user) = redis_fetch_user_by_name(&mut redis, &username)
        .await
        .map_err(|err| {
            error!("failed to fetch user from redis: {err}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to fetch user from redis: {err}"),
            )
        })?
    {
        trace!("got user from redis cache: {user:?}");
        return Ok(Json(user));
    } else {
        trace!("user not found in redis cache");
    }

    let user: User = mongo_get_user_by_username(db, &username)
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

    redis.set(username, user_json).await.map_err(|err| {
        error!("failed to insert user into redis cache: {err}");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to insert user into redis cache: {err}"),
        )
    })?;

    trace!("inserted user into redis cache");

    Ok(Json(user))
}

#[tracing::instrument(skip_all)]
async fn mongo_get_user_by_username(
    db: Database,
    username: &String,
) -> mongodb::error::Result<Option<User>> {
    db.collection("user")
        .find_one(doc! { "username": &username }, None)
        .await
}

#[tracing::instrument(skip_all)]
async fn redis_fetch_user_by_name(
    redis: &mut Connection,
    username: &String,
) -> RedisResult<Option<User>> {
    redis.get(&username).await
}

#[axum::debug_handler(state = Application)]
#[tracing::instrument(skip(db, redis, rabbitmq))]
async fn post_new_post(
    Mongo(db): Mongo,
    RabbitMQ(rabbitmq): RabbitMQ,
    Redis(mut redis): Redis,
    Path(username): Path<String>,
    Json(new_post): Json<NewPost>,
) -> Result<Json<Post>, (StatusCode, String)> {
    let post = Post {
        title: new_post.title,
        content: new_post.content,
        author: username.clone(),
        date_of_creation: OffsetDateTime::now_utc().date(),
        likes: vec![],
        comments: vec![],
    };

    db.collection("post")
        .insert_one(
            bson::to_document(&post).map_err(|err| {
                error!("failed to insert post: {err}");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("failed to insert post: {err}"),
                )
            })?,
            None,
        )
        .await
        .map_err(|err| {
            error!("failed to insert post: {err}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to insert post: {err}"),
            )
        })?;

    trace!("Inserted post: {:?}", post);

    let post_json_bytes = serde_json::to_vec(&post).map_err(|err| {
        error!("failed to serialize post: {err}");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to serialize post: {err}"),
        )
    })?;

    redis
        .set(
            format!("{username}/{}", &post.title),
            post_json_bytes.clone(),
        )
        .await
        .map_err(|err| {
            error!("failed to insert post into redis cache: {err}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to insert post into redis cache: {err}"),
            )
        })?;

    let Some(user) = db
        .collection::<User>("user")
        .find_one(doc! { "username": &username }, None)
        .await
        .map_err(|err| {
            error!("failed to find {username}: {err}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to find {username}: {err}"),
            )
        })? else {
        error!("user {username} not found");
        return Err((
            StatusCode::BAD_REQUEST,
            format!("user {username} not found"),
        ));
    };

    let channel = rabbitmq.create_channel().await.map_err(|err| {
        error!("failed to create rabbitmq channel: {err}");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to create rabbitmq channel: {err}"),
        )
    })?;

    let post_notification = Notification::NewPost(post.clone());
    let post_notification_json_bytes = serde_json::to_vec(&post_notification).map_err(|err| {
        error!("failed to serialize post notification: {err}");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to serialize post notification: {err}"),
        )
    })?;

    for follower in user.followers {
        trace!("notifying follower: {:?}", &follower);

        channel
            .queue_declare(
                &follower,
                QueueDeclareOptions::default(),
                FieldTable::default(),
            )
            .await
            .map_err(|err| {
                error!("failed to declare queue: {err}");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("failed to declare queue: {err}"),
                )
            })?;

        channel
            .basic_publish(
                "",
                &follower,
                BasicPublishOptions::default(),
                post_notification_json_bytes.as_slice(),
                BasicProperties::default(),
            )
            .await
            .map_err(|err| {
                error!("failed to publish post to friends: {err}");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("failed to publish post to friends: {err}"),
                )
            })?;
    }

    Ok(Json(post))
}

#[axum::debug_handler(state = Application)]
#[tracing::instrument(skip(db))]
async fn get_post_comment(
    Mongo(db): Mongo,
    Path((username, post_title)): Path<(String, String)>,
) -> Result<Json<Vec<Comment>>, (StatusCode, String)> {
    let post: Post = db
        .collection("post")
        .find_one(
            doc! {
                "author": &username,
                "title": &post_title,
            },
            None,
        )
        .await
        .map_err(|err| {
            error!("failed to find post: {err}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to find post: {err}"),
            )
        })?
        .ok_or_else(|| {
            error!("post {username}/{post_title} not found");
            (
                StatusCode::BAD_REQUEST,
                format!("post {username}/{post_title} not found"),
            )
        })?;

    Ok(Json(post.comments))
}

#[axum::debug_handler(state = Application)]
#[tracing::instrument(skip(db, redis))]
async fn post_new_comment(
    Mongo(db): Mongo,
    Redis(mut redis): Redis,
    Path((username, post_title)): Path<(String, String)>,
    Json(new_comment): Json<NewComment>,
) -> Result<Json<Comment>, (StatusCode, String)> {
    let comment = Comment {
        content: new_comment.content,
        author: new_comment.author,
        date_of_creation: OffsetDateTime::now_utc().date(),
        number_of_likes: 0,
    };

    let post = db
        .collection::<Post>("post")
        .find_one_and_update(
            doc! {
                "author": &username,
                "title": &post_title,
            },
            doc! {
                "$push": {
                    "comments": bson::to_document(&comment).map_err(|err| {
                        error!("failed to insert comment: {err}");
                        (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            format!("failed to insert comment: {err}"),
                        )
                    })?,
                },
            },
            FindOneAndUpdateOptions::builder()
                .return_document(ReturnDocument::After)
                .build(),
        )
        .await
        .map_err(|err| {
            error!("failed to insert comment: {err}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to insert comment: {err}"),
            )
        })?;

    let Some(post) = post else {
        error!("post {username}/{post_title} not found");
        return Err((
            StatusCode::BAD_REQUEST,
            format!("post {username}/{post_title} not found"),
        ));
    };

    trace!("Inserted comment: {:?}", comment);

    let post_json_bytes = serde_json::to_vec(&post).map_err(|err| {
        error!("failed to serialize post: {err}");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to serialize post: {err}"),
        )
    })?;

    redis
        .set(format!("{username}/{post_title}"), post_json_bytes)
        .await
        .map_err(|err| {
            error!("failed to insert post into redis cache: {err}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to insert post into redis cache: {err}"),
            )
        })?;

    Ok(Json(comment))
}

#[axum::debug_handler(state = Application)]
#[tracing::instrument(skip(rabbitmq))]
async fn get_user_notification(
    RabbitMQ(rabbitmq): RabbitMQ,
    Path(username): Path<String>,
) -> Result<Json<Vec<Notification>>, (StatusCode, String)> {
    let channel = rabbitmq.create_channel().await.map_err(|err| {
        error!("failed to create rabbitmq channel: {err}");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to create rabbitmq channel: {err}"),
        )
    })?;

    trace!("created rabbitmq channel");

    let mut vec = vec![];

    while let Some(next) = channel.basic_get(&username, BasicGetOptions::default()).await.transpose()
    {
        let next = next.map_err(|err| {
            error!("failed to get next notification: {err}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to get next notification: {err}"),
            )
        })?;

        next.ack(BasicAckOptions::default()).await.map_err(|err| {
            error!("failed to ack notification: {err}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to ack notification: {err}"),
            )
        })?;

        let parsed = serde_json::from_slice(&next.data).map_err(|err| {
            error!("failed to deserialize notification: {err}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to deserialize notification: {err}"),
            )
        })?;

        trace!("received notification: {:?}", parsed);
        vec.push(parsed);
    }

    trace!("finished consuming notifications");

    Ok(Json(vec))
}

#[axum::debug_handler(state = Application)]
#[tracing::instrument(skip(db, rabbitmq))]
async fn post_post_like(
    Mongo(db): Mongo,
    RabbitMQ(rabbitmq): RabbitMQ,
    Path((username, title)): Path<(String, String)>,
    Json(Like{ liker }): Json<Like>,
) -> Result<(), (StatusCode, String)> {
    db.collection::<Post>("post").find_one_and_update(
        doc! {
            "author": &username,
            "title": &title,
        },
        doc! {
            "$inc": {
                "number_of_likes": 1,
            }
        },
        None,
    ).await.map_err(|err| {
        error!("failed to like post: {err}");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to like post: {err}"),
        )
    })?;

    let notification_json_bytes = serde_json::to_vec(&Notification::NewPostLike(NewPostLike {
        liker,
        title,
    })).map_err(|err| {
        error!("failed to serialize notification: {err}");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to serialize notification: {err}"),
        )
    })?;

    rabbitmq.create_channel().await.map_err(|err| {
        error!("failed to create rabbitmq channel: {err}");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to create rabbitmq channel: {err}"),
        )
    })?.basic_publish(
        "",
        &username,
        BasicPublishOptions::default(),
        &notification_json_bytes,
        BasicProperties::default(),
    ).await.map_err(|err| {
        error!("failed to publish notification: {err}");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to publish notification: {err}"),
        )
    })?;

    Ok(())
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

#[tracing::instrument]
fn get_redis() -> anyhow::Result<deadpool_redis::Pool> {
    trace!("Reading REDIS_URL");
    let redis_url = std::env::var("REDIS_URL")?;
    let client = deadpool_redis::Config::from_url(redis_url).create_pool(Some(Runtime::Tokio1))?;
    trace!("Redis client created");
    Ok(client)
}

#[tracing::instrument]
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
    let config = Config {
        url: Some(rabbitmq_url),
        ..Config::default()
    };
    let pool = config.create_pool(Some(Runtime::Tokio1))?;
    trace!("RabbitMQ channel created");
    Ok(pool)
}
