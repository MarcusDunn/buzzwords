use axum::extract::{FromRef, FromRequestParts};
use axum::http::request::Parts;
use axum::http::StatusCode;
use mongodb::{Database, IndexModel};
use tracing::log::error;
use anyhow::anyhow;
use mongodb::results::CreateIndexResult;
use tracing::trace;
use mongodb::bson::doc;
use mongodb::options::{ClientOptions, IndexOptions};
use crate::comment::Comment;
use crate::post::Post;
use crate::user::User;

pub struct Mongo(pub Database);

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

pub async fn index_mongo(mongo: &mongodb::Client) -> anyhow::Result<()> {
    let database = mongo
        .default_database()
        .ok_or_else(|| anyhow!("no default database"))?;

    tokio::try_join!(index_users(&database), index_posts(&database))?;

    Ok(())
}

#[tracing::instrument]
pub async fn get_mongodb() -> anyhow::Result<mongodb::Client> {
    trace!("Reading MONGO_URL");
    let mongo_url = std::env::var("MONGO_URL")?.parse()?;
    let options = ClientOptions::parse_connection_string(mongo_url).await?;
    trace!(?options, "Creating mongo client");
    let client = mongodb::Client::with_options(options)?;
    trace!("Mongo client created");
    Ok(client)
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

async fn index_comments(database: &Database) -> mongodb::error::Result<CreateIndexResult> {
    trace!("creating comment index on post_author, post_title, and title");
    database
        .collection::<Comment>("comment")
        .create_index(
            IndexModel::builder()
                .keys(doc! { "post_author": 1, "post_title": 1, "title": 1 })
                .options(IndexOptions::builder().unique(true).build())
                .build(),
            None,
        )
        .await
}