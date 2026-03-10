pub mod postgres;
pub mod qdrant;
pub mod error;

use std::time::Duration;

use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

/// Shared database handle for all PostgreSQL operations.
#[derive(Clone)]
pub struct Store {
    pool: PgPool,
}

impl Store {
    pub async fn connect(database_url: &str) -> Result<Self, error::StoreError> {
        let pool = PgPoolOptions::new()
            .max_connections(25)
            .acquire_timeout(Duration::from_secs(10))
            .idle_timeout(Duration::from_secs(600))
            .connect(database_url)
            .await
            .map_err(|e| error::StoreError::Connection(e.to_string()))?;
        Ok(Self { pool })
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
