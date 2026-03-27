use qdrant_client::Qdrant;
use qdrant_client::qdrant::{
    CreateCollectionBuilder, SearchPointsBuilder,
    UpsertPointsBuilder, VectorParamsBuilder, DeletePointsBuilder,
    PointsIdsList, point_id::PointIdOptions,
    CreateFieldIndexCollectionBuilder, FieldType,
};

// Re-export types so callers can build filters and points without depending
// on qdrant_client directly.
pub use qdrant_client::qdrant::{Distance, Filter, Condition, Value, PointStruct};
use uuid::Uuid;

use crate::error::StoreError;

const EMBEDDING_DIM: u64 = 384;

/// Qdrant client wrapper for vector operations.
#[derive(Clone)]
pub struct VectorStore {
    client: Qdrant,
}

impl VectorStore {
    pub async fn connect(url: &str) -> Result<Self, StoreError> {
        let client = Qdrant::from_url(url)
            .build()
            .map_err(|e| StoreError::Qdrant(e.to_string()))?;
        Ok(Self { client })
    }

    /// Ensure a collection exists with the correct schema.
    pub async fn ensure_collection(&self, name: &str) -> Result<(), StoreError> {
        let exists = self
            .client
            .collection_exists(name)
            .await
            .map_err(|e| StoreError::Qdrant(e.to_string()))?;

        if !exists {
            match self
                .client
                .create_collection(
                    CreateCollectionBuilder::new(name)
                        .vectors_config(VectorParamsBuilder::new(EMBEDDING_DIM, Distance::Cosine)),
                )
                .await
            {
                Ok(_) => tracing::info!("created qdrant collection: {name}"),
                Err(e) if e.to_string().contains("already exists") => {
                    tracing::debug!("qdrant collection already exists (race): {name}");
                }
                Err(e) => return Err(StoreError::Qdrant(e.to_string())),
            }
        }
        Ok(())
    }

    /// Ensure a collection exists with a specific vector dimension and distance metric.
    ///
    /// Used for SDR collections (256-dim, Dot) which differ from the default
    /// 384-dim Cosine collection used for dense embeddings.
    pub async fn ensure_collection_dim(
        &self,
        name: &str,
        dim: u64,
        distance: Distance,
    ) -> Result<(), StoreError> {
        let exists = self
            .client
            .collection_exists(name)
            .await
            .map_err(|e| StoreError::Qdrant(e.to_string()))?;

        if !exists {
            match self
                .client
                .create_collection(
                    CreateCollectionBuilder::new(name)
                        .vectors_config(VectorParamsBuilder::new(dim, distance)),
                )
                .await
            {
                Ok(_) => tracing::info!("created qdrant collection: {name} (dim={dim})"),
                Err(e) if e.to_string().contains("already exists") => {
                    tracing::debug!("qdrant collection already exists (race): {name}");
                }
                Err(e) => return Err(StoreError::Qdrant(e.to_string())),
            }
        }
        Ok(())
    }

    /// Upsert a single vector point (with 1 retry on transient failure).
    pub async fn upsert(
        &self,
        collection: &str,
        id: Uuid,
        embedding: Vec<f32>,
        payload: std::collections::HashMap<String, Value>,
    ) -> Result<(), StoreError> {
        let point = PointStruct::new(id.to_string(), embedding.clone(), payload.clone());
        match self
            .client
            .upsert_points(UpsertPointsBuilder::new(collection, vec![point]))
            .await
        {
            Ok(_) => return Ok(()),
            Err(first_err) => {
                tracing::warn!(error = %first_err, "qdrant upsert failed, retrying in 500ms");
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                let retry_point = PointStruct::new(id.to_string(), embedding, payload);
                self.client
                    .upsert_points(UpsertPointsBuilder::new(collection, vec![retry_point]))
                    .await
                    .map_err(|e| StoreError::Qdrant(e.to_string()))?;
            }
        }
        Ok(())
    }

    /// Search for nearest vectors. Returns (id, score) pairs (with 1 retry).
    pub async fn search(
        &self,
        collection: &str,
        query_embedding: Vec<f32>,
        limit: u64,
    ) -> Result<Vec<(Uuid, f32)>, StoreError> {
        let do_search = |emb: Vec<f32>| async {
            self.client
                .search_points(
                    SearchPointsBuilder::new(collection, emb, limit)
                        .with_payload(false),
                )
                .await
        };

        let results = match do_search(query_embedding.clone()).await {
            Ok(r) => r,
            Err(first_err) => {
                tracing::warn!(error = %first_err, "qdrant search failed, retrying in 500ms");
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                do_search(query_embedding)
                    .await
                    .map_err(|e| StoreError::Qdrant(e.to_string()))?
            }
        };

        let mut out = Vec::with_capacity(results.result.len());
        for point in results.result {
            let id_str = match point.id.as_ref().and_then(|pid| pid.point_id_options.as_ref()) {
                Some(PointIdOptions::Uuid(s)) => s.clone(),
                _ => continue,
            };
            if let Ok(id) = Uuid::parse_str(&id_str) {
                out.push((id, point.score));
            }
        }
        Ok(out)
    }

    /// Delete multiple points by UUID in a single request (efficient batch cleanup).
    ///
    /// OPTIMIZATION: Sends a single PointsIdsList delete rather than N individual requests.
    /// Fallback: if `ids` is empty, returns immediately without making a network call.
    pub async fn delete_many(&self, collection: &str, ids: &[Uuid]) -> Result<(), StoreError> {
        if ids.is_empty() {
            return Ok(());
        }
        let point_ids: Vec<qdrant_client::qdrant::PointId> =
            ids.iter().map(|id| id.to_string().into()).collect();
        self.client
            .delete_points(
                DeletePointsBuilder::new(collection)
                    .points(PointsIdsList { ids: point_ids }),
            )
            .await
            .map_err(|e| StoreError::Qdrant(e.to_string()))?;
        Ok(())
    }

    /// Delete an entire collection. Returns Ok(()) if the collection does not exist.
    pub async fn delete_collection(&self, name: &str) -> Result<(), StoreError> {
        match self.client.delete_collection(name).await {
            Ok(_) => {
                tracing::info!("deleted qdrant collection: {name}");
                Ok(())
            }
            Err(e) if e.to_string().contains("not found") || e.to_string().contains("doesn't exist") => {
                tracing::debug!("qdrant collection not found (already deleted): {name}");
                Ok(())
            }
            Err(e) => Err(StoreError::Qdrant(e.to_string())),
        }
    }

    /// Delete a point by UUID.
    pub async fn delete(&self, collection: &str, id: Uuid) -> Result<(), StoreError> {
        self.client
            .delete_points(
                DeletePointsBuilder::new(collection)
                    .points(PointsIdsList {
                        ids: vec![id.to_string().into()],
                    }),
            )
            .await
            .map_err(|e| StoreError::Qdrant(e.to_string()))?;
        Ok(())
    }

    /// Search with an optional payload filter. Returns (id, score) pairs.
    ///
    /// Qdrant pre-filters payloads before HNSW traversal at zero latency cost.
    /// Pass `None` for unfiltered search (backward compat).
    pub async fn search_filtered(
        &self,
        collection: &str,
        query_embedding: Vec<f32>,
        limit: u64,
        filter: Option<Filter>,
    ) -> Result<Vec<(Uuid, f32)>, StoreError> {
        let do_search = |emb: Vec<f32>| {
            let mut builder = SearchPointsBuilder::new(collection, emb, limit)
                .with_payload(false);
            if let Some(ref f) = filter {
                builder = builder.filter(f.clone());
            }
            async move {
                self.client.search_points(builder).await
            }
        };

        let results = match do_search(query_embedding.clone()).await {
            Ok(r) => r,
            Err(first_err) => {
                tracing::warn!(error = %first_err, "qdrant filtered search failed, retrying in 500ms");
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                do_search(query_embedding)
                    .await
                    .map_err(|e| StoreError::Qdrant(e.to_string()))?
            }
        };

        let mut out = Vec::with_capacity(results.result.len());
        for point in results.result {
            let id_str = match point.id.as_ref().and_then(|pid| pid.point_id_options.as_ref()) {
                Some(PointIdOptions::Uuid(s)) => s.clone(),
                _ => continue,
            };
            if let Ok(id) = Uuid::parse_str(&id_str) {
                out.push((id, point.score));
            }
        }
        Ok(out)
    }

    /// Create a keyword payload index on a field for O(1) pre-filtering.
    ///
    /// Idempotent — Qdrant ignores duplicate index creation.
    pub async fn create_payload_index(
        &self,
        collection: &str,
        field_name: &str,
    ) -> Result<(), StoreError> {
        self.client
            .create_field_index(
                CreateFieldIndexCollectionBuilder::new(
                    collection,
                    field_name,
                    FieldType::Keyword,
                ),
            )
            .await
            .map_err(|e| StoreError::Qdrant(e.to_string()))?;
        tracing::info!(collection, field_name, "qdrant payload index created");
        Ok(())
    }

    /// Batch upsert multiple points in a single request.
    ///
    /// Use for migrations — avoids per-point network round-trips over Munin→Hades.
    /// Caller builds `Vec<PointStruct>` in memory and fires one gRPC call per batch.
    pub async fn upsert_batch(
        &self,
        collection: &str,
        points: Vec<PointStruct>,
    ) -> Result<(), StoreError> {
        if points.is_empty() {
            return Ok(());
        }
        self.client
            .upsert_points(UpsertPointsBuilder::new(collection, points))
            .await
            .map_err(|e| StoreError::Qdrant(e.to_string()))?;
        Ok(())
    }
}
