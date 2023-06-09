use serde::{Deserialize, Serialize};
use time::{Date, OffsetDateTime};
use axum::extract::Path;
use axum::Json;
use axum::http::StatusCode;
use futures::StreamExt;
use mongodb::bson::doc;
use tracing::{error, trace};
use mongodb::options::{FindOneAndUpdateOptions, ReturnDocument};
use redis::AsyncCommands;
use crate::ampq::Ampq;
use crate::mongo::Mongo;
use crate::notification::{NewCommentLike, Notification};
use crate::post::{Like, Post};
use crate::redis::Redis;

#[derive(Debug, Deserialize, Clone)]
pub struct NewComment {
    pub title: String,
    pub content: String,
    pub author: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Comment {
    pub post_author: String,
    pub post_title: String,
    pub title: String,
    pub content: String,
    pub author: String,
    pub date_of_creation: Date,
    pub likers: Vec<String>,
}

#[axum::debug_handler(state = crate::Application)]
#[tracing::instrument(skip(db, redis))]
pub async fn get_post_comment(
    Mongo(db): Mongo,
    Redis(mut redis): Redis,
    Path((username, post_title)): Path<(String, String)>,
) -> Result<Json<Vec<Comment>>, (StatusCode, String)> {
    let post = redis.get::<_, Option<Post>>(format!("{username}/{post_title}")).await.map_err(|err| {
        error!("failed to get post from redis: {err}");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to get post from redis: {err}"),
        )
    })?;

    let comments = match post {
        Some(post) => post.comments,
        None => {
            db
                .collection::<Post>("post")
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
                        StatusCode::NOT_FOUND,
                        format!("post {username}/{post_title} not found"),
                    )
                })?.comments
        }
    };

    let mut comments = db.collection::<Comment>("comments")
        .find(
            doc! {
                    "post_author": &username,
                    "post_title": &post_title,
                },
            None,
        ).await
        .map_err(|err| {
            error!("failed to find comments: {err}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to find comments: {err}"),
            )
        })?;

    let mut collector = vec![];

    while let Some(comment) = comments.next().await {
        collector.push(comment.map_err(|err| {
            error!("failed to get comment: {err}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to get comment: {err}"),
            )
        })?);
    }

    Ok(Json(collector))
}

#[axum::debug_handler(state = crate::Application)]
#[tracing::instrument(skip(db, redis, rabbitmq))]
pub async fn post_new_comment(
    Mongo(db): Mongo,
    Redis(mut redis): Redis,
    Ampq(rabbitmq): Ampq,
    Path((username, post_title)): Path<(String, String)>,
    Json(new_comment): Json<NewComment>,
) -> Result<Json<Comment>, (StatusCode, String)> {
    let comment = Comment {
        post_title: post_title.clone(),
        title: new_comment.title,
        post_author: username.clone(),
        content: new_comment.content,
        author: new_comment.author,
        date_of_creation: OffsetDateTime::now_utc().date(),
        likers: vec![],
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
                    "comments": &comment.title
                },
            },
            FindOneAndUpdateOptions::builder()
                .return_document(ReturnDocument::After)
                .build(),
        )
        .await
        .map_err(|err| {
            error!("failed to insert comment into post: {err}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to insert comment into post: {err}"),
            )
        })?;

    db
        .collection("comment")
        .insert_one(mongodb::bson::to_document(&comment).map_err(|err| {
            error!("failed to serialize comment: {err}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to serialize comment: {err}"),
            )
        })?, None)
        .await
        .map_err(|err| {
            error!("failed to insert comment into mongo: {err}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to insert comment into mongo: {err}"),
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

    let channel = rabbitmq.create_channel().await.map_err(|err| {
        error!("failed to create rabbitmq channel: {err}");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to create rabbitmq channel: {err}"),
        )
    })?;

    let notification_json_bytes = serde_json::to_vec(&Notification::NewComment(comment.clone())).map_err(|err| {
        error!("failed to serialize notification: {err}");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to serialize notification: {err}"),
        )
    })?;

    channel
        .basic_publish(
            "",
            &username,
            lapin::options::BasicPublishOptions::default(),
            &notification_json_bytes,
            lapin::BasicProperties::default(),
        )
        .await
        .map_err(|err| {
            error!("failed to publish notification: {err}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to publish notification: {err}"),
            )
        })?;

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

#[axum::debug_handler(state = crate::Application)]
#[tracing::instrument(skip(db, redis, rabbitmq))]
pub async fn post_comment_like(
    Mongo(db): Mongo,
    Ampq(rabbitmq): Ampq,
    Redis(mut redis): Redis,
    Path((user_id, post_title, comment_title)): Path<(String, String, String)>,
    Json(Like { liker }): Json<Like>,
) -> Result<Json<Comment>, (StatusCode, String)> {
    let Some(comment) = db.collection::<Comment>("comment")
        .find_one_and_update(
            doc! {
                "post_title": &post_title,
                "post_author": &user_id,
                "title": &comment_title,
            },
            doc! {
                "$addToSet": {
                    "likers": &liker,
                },
            },
            FindOneAndUpdateOptions::builder()
                .return_document(ReturnDocument::After)
                .build(),
        ).await.map_err(|err| {
            error!("failed to like comment: {err}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to like comment: {err}"),
            )
        })? else {
            error!("comment {user_id}/{post_title}/{comment_title} not found");
            return Err((
                StatusCode::NOT_FOUND,
                format!("comment {user_id}/{post_title}/{comment_title} not found"),
            ));
        };

    trace!("Liked comment: {:?}", comment);

    let channel = rabbitmq.create_channel().await.map_err(|err| {
        error!("failed to create rabbitmq channel: {err}");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to create rabbitmq channel: {err}"),
        )
    })?;

    let notification_json_bytes = serde_json::to_vec(&Notification::NewCommentLike(NewCommentLike {
        comment: comment.clone(),
        liker,
    })).map_err(|err| {
        error!("failed to serialize notification: {err}");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to serialize notification: {err}"),
        )
    })?;

    channel
        .basic_publish(
            "",
            &comment.author,
            lapin::options::BasicPublishOptions::default(),
            &notification_json_bytes,
            lapin::BasicProperties::default(),
        )
        .await
        .map_err(|err| {
            error!("failed to publish notification: {err}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to publish notification: {err}"),
            )
        })?;

    trace!("notified user {} of new comment like", &comment.author);

    let comment_json_bytes = serde_json::to_vec(&comment).map_err(|err| {
        error!("failed to serialize comment: {err}");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to serialize comment: {err}"),
        )
    })?;

    Ok(Json(comment))
}

