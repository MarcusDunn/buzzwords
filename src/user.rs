use serde::{Deserialize, Serialize};
use redis::{AsyncCommands, FromRedisValue, RedisResult, Value};
use tracing::{error, trace};
use time::{Date, OffsetDateTime};
use axum::Json;
use axum::http::StatusCode;
use mongodb::Database;
use mongodb::bson::doc;
use deadpool_redis::Connection;
use axum::extract::Path;
use mongodb::bson::oid::ObjectId;
use crate::mongo::Mongo;
use crate::redis::Redis;

#[derive(Debug, Deserialize, Clone)]
pub struct NewUser {
    pub name: String,
    pub username: String,
    pub email: String,
    pub password: String,
}

#[derive(Debug, Serialize, Clone, Deserialize)]
pub struct User {
    pub name: String,
    pub username: String,
    pub password: String,
    pub email: String,
    pub date_of_creation: Date,
    // username
    pub followers: Vec<String>,
    // username
    pub following: Vec<String>,
    // post_title
    pub posts: Vec<String>,
    // (author, post_title)
    pub likes_posts: Vec<(String, String)>,
    // (author, post_title, comment_id)
    pub likes_comments: Vec<(String, String, ObjectId)>,
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

#[axum::debug_handler(state = crate::Application)]
#[tracing::instrument(skip(db))]
pub async fn post_new_user(
    Mongo(db): Mongo,
    Json(new_user): Json<NewUser>,
) -> Result<Json<User>, (StatusCode, String)> {
    let user = User {
        name: new_user.name,
        username: new_user.username,
        email: new_user.email,
        password: new_user.password,
        date_of_creation: OffsetDateTime::now_utc().date(),
        posts: Vec::new(),
        followers: Vec::new(),
        following: Vec::new(),
        likes_posts: Vec::new(),
        likes_comments: Vec::new(),
    };

    db.collection("user")
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

#[axum::debug_handler(state = crate::Application)]
#[tracing::instrument(skip(db, redis))]
pub async fn get_user(
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

