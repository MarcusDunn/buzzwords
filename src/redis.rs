use axum::extract::{FromRef, FromRequestParts};
use axum::http::StatusCode;
use axum::http::request::Parts;
use tracing::{error, trace};
use deadpool_redis::Connection;
use deadpool_lapin::Runtime;

pub struct Redis(pub Connection);

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

#[tracing::instrument]
pub fn get_redis() -> anyhow::Result<deadpool_redis::Pool> {
    trace!("Reading REDIS_URL");
    let redis_url = std::env::var("REDIS_URL")?;
    let client = deadpool_redis::Config::from_url(redis_url).create_pool(Some(Runtime::Tokio1))?;
    trace!("Redis client created");
    Ok(client)
}

