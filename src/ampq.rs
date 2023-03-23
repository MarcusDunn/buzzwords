use axum::extract::{FromRef, FromRequestParts};
use axum::http::StatusCode;
use axum::http::request::Parts;
use tracing::{error, trace};

pub struct Ampq(pub deadpool_lapin::Connection);

#[axum::async_trait]
impl<S> FromRequestParts<S> for Ampq
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
        Ok(Ampq(rabbit_conn))
    }
}

#[tracing::instrument]
pub async fn get_rabbitmq() -> anyhow::Result<deadpool_lapin::Pool> {
    use deadpool_lapin::{Config, Runtime};
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

