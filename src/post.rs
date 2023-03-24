use serde::{Deserialize, Serialize};
use time::{Date, OffsetDateTime};
use axum::extract::Path;
use axum::Json;
use axum::http::StatusCode;
use tracing::{error, trace};
use mongodb::bson::doc;
use lapin::options::{BasicPublishOptions, QueueDeclareOptions};
use lapin::types::FieldTable;
use lapin::BasicProperties;
use mongodb::options::FindOneAndUpdateOptions;
use mongodb::options::ReturnDocument::After;
use redis::{AsyncCommands, FromRedisValue, RedisResult, Value};
use redis::Value::Data;
use crate::ampq::Ampq;
use crate::mongo::Mongo;
use crate::notification::{NewPostLike, Notification};
use crate::redis::Redis;
use crate::user::User;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Post {
    pub title: String,
    pub content: String,
    pub author: String,
    pub date_of_creation: Date,
    // usernames
    pub likes: Vec<String>,
    // comment titles
    pub comments: Vec<String>,
}

impl FromRedisValue for Post {
    fn from_redis_value(v: &Value) -> RedisResult<Self> {
        if let Data(ref bytes) = v {
            Ok(serde_json::from_slice(bytes).map_err(|err| {
                redis::RedisError::from((redis::ErrorKind::Serialize, "error deserializing into post", format!("{err}")))
            })?)
        } else {
            Err(redis::RedisError::from((redis::ErrorKind::TypeError, "wrong type")))
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct NewPost {
    pub title: String,
    pub content: String,
}

#[axum::debug_handler(state = crate::Application)]
#[tracing::instrument(skip(db, redis, rabbitmq))]
pub async fn post_new_post(
    Mongo(db): Mongo,
    Ampq(rabbitmq): Ampq,
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
            mongodb::bson::to_document(&post).map_err(|err| {
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

    let Some(user) = db.collection::<User>("user")
        .find_one_and_update(
            doc! { "username": &username },
            doc! { "$push": { "posts": &post.title } },
            Some(FindOneAndUpdateOptions::builder().return_document(After).build()),
        ).await.map_err(|err| {
        error!("failed to update user");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to update user"),
        )
    })? else {
        error!("user {username} not found");
        return Err((
            StatusCode::BAD_REQUEST,
            format!("user {username} not found"),
        ));
    };

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

    let user_json_bytes = serde_json::to_vec(&user).map_err(|err| {
        error!("failed to serialize user: {err}");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to serialize user: {err}"),
        )
    })?;

    redis.set(&user.username, user_json_bytes)
        .await
        .map_err(|err| {
            error!("failed to insert user into redis cache: {err}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to insert user into redis cache: {err}"),
            )
        })?;

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

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Like {
    pub liker: String,
}

#[axum::debug_handler(state = crate::Application)]
#[tracing::instrument(skip(db, rabbitmq))]
pub async fn post_post_like(
    Mongo(db): Mongo,
    Ampq(rabbitmq): Ampq,
    Path((username, title)): Path<(String, String)>,
    Json(Like { liker }): Json<Like>,
) -> Result<(), (StatusCode, String)> {
    let Some(liker) = db.collection::<User>("user").find_one_and_update(doc! {
        "username": &username,
    }, doc! {
        "$push": {
            "likes_posts": [&username, &title],
        }
    }, Some(
        FindOneAndUpdateOptions::builder().return_document(After).build()
    )).await.map_err(|err| {
        error!("failed to like post: {err}");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to like post: {err}"),
        )
    })? else {
        error!("user {liker} not found");
        return Err((
            StatusCode::BAD_REQUEST,
            format!("user {liker} not found"),
        ));
    };

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
        liker: liker.username,
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

