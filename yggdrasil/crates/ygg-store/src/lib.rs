pub mod postgres;
pub mod qdrant;
pub mod error;

use std::time::Duration;

use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use ygg_domain::config::DatabaseConfig;

/// Shared database handle for all PostgreSQL operations.
#[derive(Clone)]
pub struct Store {
    pool: PgPool,
}

impl Store {
    /// Connect using a `DatabaseConfig` with per-service pool tuning.
    pub async fn connect_with_config(config: &DatabaseConfig) -> Result<Self, error::StoreError> {
        let pool = PgPoolOptions::new()
            .max_connections(config.max_connections)
            .acquire_timeout(Duration::from_secs(config.acquire_timeout_secs))
            .idle_timeout(Duration::from_secs(config.idle_timeout_secs))
            .connect(&config.url)
            .await
            .map_err(|e| error::StoreError::Connection(e.to_string()))?;
        Ok(Self { pool })
    }

    /// Connect with default pool settings. Prefer `connect_with_config` for
    /// production services to control per-service connection limits.
    pub async fn connect(database_url: &str) -> Result<Self, error::StoreError> {
        Self::connect_with_config(&DatabaseConfig::from_url(database_url)).await
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Run migrations from a directory path (resolved at runtime).
    pub async fn migrate(&self, migrations_path: &str) -> Result<(), error::StoreError> {
        let migrator = sqlx::migrate::Migrator::new(std::path::Path::new(migrations_path))
            .await
            .map_err(|e| error::StoreError::Migration(e.to_string()))?;
        migrator
            .run(&self.pool)
            .await
            .map_err(|e| error::StoreError::Migration(e.to_string()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn connect_fails_with_invalid_url() {
        let result = Store::connect("postgres://invalid:5432/nope").await;
        assert!(result.is_err());
        let err = result.err().unwrap();
        assert!(matches!(err, error::StoreError::Connection(_)));
    }
}
