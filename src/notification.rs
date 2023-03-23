use serde::{Deserialize, Serialize};
use axum::extract::Path;
use axum::Json;
use axum::http::StatusCode;
use tracing::{error, trace};
use lapin::options::{BasicAckOptions, BasicGetOptions};
use crate::ampq::Ampq;
use crate::comment::Comment;
use crate::post::Post;


#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum Notification {
    NewPost(Post),
    NewPostLike(NewPostLike),
    NewFollower(NewFollower),
    NewComment(Comment),
    NewCommentLike(NewCommentLike),
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct NewCommentLike {
    pub comment: Comment,
    pub liker: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct NewFollower {
    pub username: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct NewPostLike {
    pub title: String,
    pub liker: String,
}

#[axum::debug_handler(state = crate::Application)]
#[tracing::instrument(skip(rabbitmq))]
pub async fn get_user_notification(
    Ampq(rabbitmq): Ampq,
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

