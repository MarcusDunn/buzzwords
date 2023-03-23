use serde::{Deserialize, Serialize};
use axum::extract::Path;
use axum::Json;
use axum::http::StatusCode;
use mongodb::bson::doc;
use mongodb::options::{FindOneAndUpdateOptions, ReturnDocument};
use tracing::{error, trace};
use redis::AsyncCommands;
use crate::mongo::Mongo;
use crate::redis::Redis;
use crate::user::User;
use crate::ampq::Ampq;
use crate::notification::{NewFollower, Notification};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Follow {
    pub follower: String,
}

#[axum::debug_handler(state = crate::Application)]
#[tracing::instrument(skip(db, redis))]
pub async fn post_user_follow(
    Mongo(db): Mongo,
    Redis(mut redis): Redis,
    Ampq(rabbitmq): Ampq,
    Path(username): Path<String>,
    Json(Follow { follower }): Json<Follow>,
) -> Result<(), (StatusCode, String)> {
    let Some(user) = db
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
        })? else {
        error!("user not found: {username}");
        return Err((
            StatusCode::NOT_FOUND,
            format!("user not found: {username}"),
        ));
    };

    let new_follower_json_bytes = serde_json::to_vec(&Notification::NewFollower(NewFollower {
        username: follower.clone(),
    })).map_err(|err| {
        error!("failed to serialize new follower: {err}");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to serialize new follower: {err}"),
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
        &user.username,
        lapin::options::BasicPublishOptions::default(),
        &new_follower_json_bytes,
        lapin::BasicProperties::default(),
    ).await.map_err(|err| {
        error!("failed to publish to rabbitmq: {err}");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to publish to rabbitmq: {err}"),
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
