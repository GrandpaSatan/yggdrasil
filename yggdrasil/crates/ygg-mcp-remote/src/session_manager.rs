//! PG-backed session manager that wraps rmcp's LocalSessionManager.
//!
//! Active session transport (SSE channels, event caches, message routing) remains
//! in-memory via LocalSessionManager — that complexity cannot be persisted.
//!
//! What PG adds:
//! - Session metadata survives restarts (project_id, client_name, state_json)
//! - On new session creation, the most recent session for the same project is
//!   looked up and its state_json is returned for context carryover
//! - Background cleanup of expired sessions (24h TTL)
//! - `last_seen` is updated on every request (touch)

use futures::Stream;
use rmcp::{
    model::{ClientJsonRpcMessage, ServerJsonRpcMessage},
    transport::{
        WorkerTransport,
        common::server_side_http::{ServerSseMessage, SessionId},
        streamable_http_server::session::{SessionManager, local::{LocalSessionManager, LocalSessionManagerError, LocalSessionWorker}},
    },
};
use tracing::{info, warn};
use ygg_store::Store;

/// Wraps LocalSessionManager with PostgreSQL session metadata persistence.
pub struct PersistentSessionManager {
    local: LocalSessionManager,
    store: Store,
    project_id: Option<String>,
}

#[allow(dead_code)]
#[derive(Debug, thiserror::Error)]
pub enum PersistentSessionManagerError {
    #[error("{0}")]
    Local(#[from] LocalSessionManagerError),
    #[error("database error: {0}")]
    Database(String),
}

impl PersistentSessionManager {
    pub fn new(store: Store, project_id: Option<String>) -> Self {
        Self {
            local: LocalSessionManager::default(),
            store,
            project_id,
        }
    }

    /// Spawn a background task that cleans expired sessions every 5 minutes.
    pub fn spawn_cleanup_task(store: Store, ct: tokio_util::sync::CancellationToken) {
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(300));
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        match ygg_store::postgres::sessions::cleanup_expired(store.pool()).await {
                            Ok(n) if n > 0 => info!(removed = n, "expired sessions cleaned"),
                            Ok(_) => {}
                            Err(e) => warn!(error = %e, "session cleanup failed"),
                        }
                    }
                    _ = ct.cancelled() => break,
                }
            }
        });
    }

    fn parse_session_uuid(id: &SessionId) -> Option<uuid::Uuid> {
        uuid::Uuid::parse_str(id.as_ref()).ok()
    }

    /// Fire-and-forget PG touch (updates last_seen).
    fn touch_bg(&self, id: &SessionId) {
        if let Some(uuid) = Self::parse_session_uuid(id) {
            let store = self.store.clone();
            tokio::spawn(async move {
                let _ = ygg_store::postgres::sessions::touch_session(store.pool(), uuid).await;
            });
        }
    }
}

impl SessionManager for PersistentSessionManager {
    type Error = PersistentSessionManagerError;
    type Transport = WorkerTransport<LocalSessionWorker>;

    async fn create_session(&self) -> Result<(SessionId, Self::Transport), Self::Error> {
        let (id, transport) = self.local.create_session().await?;

        // Persist to PG
        if let Some(uuid) = Self::parse_session_uuid(&id) {
            let pool = self.store.pool().clone();
            let project_id = self.project_id.clone();

            // Look up previous session context for same project (carry over SDR state)
            let prev_state = if let Some(ref project) = project_id {
                match ygg_store::postgres::sessions::get_latest_session_for_project(&pool, project).await {
                    Ok(Some(prev)) => {
                        info!(
                            prev_session = %prev.session_id,
                            project = project,
                            "carrying over session context from previous session"
                        );
                        Some(prev.state_json)
                    }
                    Ok(None) => None,
                    Err(e) => {
                        warn!(error = %e, "failed to look up previous session");
                        None
                    }
                }
            } else {
                None
            };

            tokio::spawn(async move {
                let _ = ygg_store::postgres::sessions::create_session(
                    &pool,
                    uuid,
                    "claude-code",
                    project_id.as_deref(),
                )
                .await;

                // Carry over previous state to new session
                if let Some(state) = prev_state {
                    let _ = ygg_store::postgres::sessions::update_state(&pool, uuid, &state).await;
                }
            });
        }

        Ok((id, transport))
    }

    async fn initialize_session(
        &self,
        id: &SessionId,
        message: ClientJsonRpcMessage,
    ) -> Result<ServerJsonRpcMessage, Self::Error> {
        self.touch_bg(id);
        Ok(self.local.initialize_session(id, message).await?)
    }

    async fn has_session(&self, id: &SessionId) -> Result<bool, Self::Error> {
        // Only check local — transport state can't be reconstructed from PG
        Ok(self.local.has_session(id).await?)
    }

    async fn close_session(&self, id: &SessionId) -> Result<(), Self::Error> {
        self.local.close_session(id).await?;
        if let Some(uuid) = Self::parse_session_uuid(id) {
            let store = self.store.clone();
            tokio::spawn(async move {
                let _ = ygg_store::postgres::sessions::delete_session(store.pool(), uuid).await;
            });
        }
        Ok(())
    }

    async fn create_stream(
        &self,
        id: &SessionId,
        message: ClientJsonRpcMessage,
    ) -> Result<impl Stream<Item = ServerSseMessage> + Send + Sync + 'static, Self::Error> {
        self.touch_bg(id);
        Ok(self.local.create_stream(id, message).await?)
    }

    async fn accept_message(
        &self,
        id: &SessionId,
        message: ClientJsonRpcMessage,
    ) -> Result<(), Self::Error> {
        self.touch_bg(id);
        Ok(self.local.accept_message(id, message).await?)
    }

    async fn create_standalone_stream(
        &self,
        id: &SessionId,
    ) -> Result<impl Stream<Item = ServerSseMessage> + Send + Sync + 'static, Self::Error> {
        self.touch_bg(id);
        Ok(self.local.create_standalone_stream(id).await?)
    }

    async fn resume(
        &self,
        id: &SessionId,
        last_event_id: String,
    ) -> Result<impl Stream<Item = ServerSseMessage> + Send + Sync + 'static, Self::Error> {
        self.touch_bg(id);
        Ok(self.local.resume(id, last_event_id).await?)
    }
}
