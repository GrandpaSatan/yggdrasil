//! One-time migration: classify engrams by project using LFM2-24B, then
//! backfill PG `project`/`scope` columns and batch upsert into `yggdrasil_v2_sdr`
//! with bipolar vectors + project/scope payloads.
//!
//! Usage:
//!   mimir-migrate-v2 --database-url postgres://... --qdrant-url http://... --ollama-url http://...
//!
//! The migration is idempotent — engrams already assigned a project are skipped.

use std::collections::HashMap;

use clap::Parser;
use reqwest::Client;
use sqlx::{PgPool, Row as _};
use uuid::Uuid;

use ygg_store::qdrant::{Distance, Value, VectorStore};

use mimir::sdr;
use mimir::state::V2_SDR_COLLECTION;

/// LFM2-24B model identifier on Ollama.
const LLM_MODEL: &str = "hf.co/LiquidAI/LFM2-24B-A2B-GGUF:Q4_K_M";

/// Batch size for Qdrant upserts (avoid network saturation over Munin→Hades).
const QDRANT_BATCH_SIZE: usize = 500;

/// Number of engrams to classify per LLM call.
const LLM_BATCH_SIZE: usize = 10;

#[derive(Parser)]
#[command(name = "mimir-migrate-v2", about = "Migrate engrams to project-isolated v2 collection")]
struct Args {
    /// PostgreSQL connection URL.
    #[arg(long, env = "MIMIR_DATABASE_URL")]
    database_url: String,

    /// Qdrant gRPC URL (e.g. http://localhost:6334).
    #[arg(long, env = "QDRANT_URL")]
    qdrant_url: String,

    /// Ollama API URL (e.g. http://localhost:11434).
    #[arg(long, env = "OLLAMA_URL", default_value = "http://localhost:11434")]
    ollama_url: String,

    /// Dry run — classify but don't write to PG or Qdrant.
    #[arg(long, default_value_t = false)]
    dry_run: bool,

    /// Skip LLM classification and assign all unclassified engrams to this project.
    #[arg(long)]
    default_project: Option<String>,
}

/// Row fetched from PG for migration.
struct EngramRow {
    id: Uuid,
    cause: String,
    effect: String,
    tags: Vec<String>,
    sdr_bits: Option<Vec<u8>>,
    project: Option<String>,
}

#[derive(serde::Deserialize)]
struct OllamaResponse {
    response: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();
    let http = Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()?;

    // --- Connect to PG ---
    tracing::info!("connecting to postgresql");
    let pool = PgPool::connect(&args.database_url).await?;

    // --- Connect to Qdrant ---
    tracing::info!("connecting to qdrant at {}", args.qdrant_url);
    let vectors = VectorStore::connect(&args.qdrant_url).await?;

    // Ensure v2 collection exists with payload indexes
    vectors
        .ensure_collection_dim(V2_SDR_COLLECTION, 256, Distance::Dot)
        .await?;
    vectors.create_payload_index(V2_SDR_COLLECTION, "project").await?;
    vectors.create_payload_index(V2_SDR_COLLECTION, "scope").await?;
    tracing::info!("v2 collection ready");

    // --- Fetch all engrams ---
    tracing::info!("loading engrams from postgresql");
    let rows: Vec<_> = sqlx::query(
        "SELECT id, cause, effect, tags, sdr_bits, project \
         FROM yggdrasil.engrams \
         ORDER BY created_at ASC",
    )
    .fetch_all(&pool)
    .await?;

    let engrams: Vec<EngramRow> = rows
        .into_iter()
        .map(|r| EngramRow {
            id: r.get("id"),
            cause: r.get("cause"),
            effect: r.get("effect"),
            tags: r.get::<Vec<String>, _>("tags"),
            sdr_bits: r.get::<Option<Vec<u8>>, _>("sdr_bits"),
            project: r.get::<Option<String>, _>("project"),
        })
        .collect();

    let total = engrams.len();
    tracing::info!(total, "engrams loaded");

    // --- Partition: already classified vs needs classification ---
    let (already_done, needs_work): (Vec<_>, Vec<_>) =
        engrams.into_iter().partition(|e| e.project.is_some());

    tracing::info!(
        already_classified = already_done.len(),
        needs_classification = needs_work.len(),
        "partition complete"
    );

    // --- Phase 1: Classify unassigned engrams ---
    let mut classifications: Vec<(Uuid, String)> = Vec::with_capacity(needs_work.len());

    if let Some(ref default_proj) = args.default_project {
        // Skip LLM — assign everything to default project
        tracing::info!(project = %default_proj, "using default project for all unclassified engrams");
        for e in &needs_work {
            classifications.push((e.id, default_proj.clone()));
        }
    } else {
        // Use LFM2-24B to classify
        tracing::info!(model = LLM_MODEL, "classifying engrams via LLM");

        for (batch_idx, chunk) in needs_work.chunks(LLM_BATCH_SIZE).enumerate() {
            let mut entries = String::new();
            for (i, e) in chunk.iter().enumerate() {
                let cause_preview: String = e.cause.chars().take(150).collect();
                let effect_preview: String = e.effect.chars().take(150).collect();
                let tags_str = e.tags.join(", ");
                entries.push_str(&format!(
                    "{}. ID={}\n   Cause: {}\n   Effect: {}\n   Tags: [{}]\n\n",
                    i + 1,
                    e.id,
                    cause_preview,
                    effect_preview,
                    tags_str
                ));
            }

            let prompt = format!(
                "You are classifying memory entries into projects. \
                 For each entry below, respond with ONLY the entry number and project name, \
                 one per line, in the format: `N: project_name`\n\n\
                 Known projects: yggdrasil, fenrir\n\
                 If an entry is about infrastructure, networking, hardware, deployment, \
                 or doesn't belong to a specific project, use: global\n\n\
                 Entries:\n{entries}\
                 \nRespond with ONLY the classifications, nothing else:"
            );

            let body = serde_json::json!({
                "model": LLM_MODEL,
                "prompt": prompt,
                "stream": false,
                "options": {
                    "temperature": 0.1,
                    "num_predict": 256
                }
            });

            match http
                .post(format!("{}/api/generate", args.ollama_url))
                .json(&body)
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() => {
                    let ollama: OllamaResponse = resp.json().await?;
                    let parsed = parse_classifications(&ollama.response, chunk);
                    classifications.extend(parsed);
                }
                Ok(resp) => {
                    tracing::warn!(
                        batch = batch_idx,
                        status = %resp.status(),
                        "LLM returned error, assigning batch to global"
                    );
                    for e in chunk {
                        classifications.push((e.id, "global".to_string()));
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        batch = batch_idx,
                        error = %e,
                        "LLM request failed, assigning batch to global"
                    );
                    for e in chunk {
                        classifications.push((e.id, "global".to_string()));
                    }
                }
            }

            if (batch_idx + 1) % 10 == 0 {
                tracing::info!(
                    batches = batch_idx + 1,
                    classified = classifications.len(),
                    total = needs_work.len(),
                    "classification progress"
                );
            }
        }
    }

    tracing::info!(classified = classifications.len(), "classification complete");

    // --- Phase 2: Backfill PG project/scope columns ---
    if !args.dry_run {
        tracing::info!("backfilling PG project/scope columns");
        for (id, project) in &classifications {
            let (pg_project, scope): (Option<&str>, &str) = if project == "global" {
                (None, "global")
            } else {
                (Some(project.as_str()), "project")
            };

            sqlx::query(
                "UPDATE yggdrasil.engrams SET project = $1, scope = $2 WHERE id = $3",
            )
            .bind(pg_project)
            .bind(scope)
            .bind(id)
            .execute(&pool)
            .await?;
        }

        // Also update already-classified engrams that have project but no scope
        sqlx::query(
            "UPDATE yggdrasil.engrams SET scope = 'project' \
             WHERE project IS NOT NULL AND scope = 'global'",
        )
        .execute(&pool)
        .await?;

        tracing::info!("PG backfill complete");
    } else {
        tracing::info!("DRY RUN — skipping PG writes");
        for (id, project) in &classifications {
            tracing::info!(id = %id, project = %project, "would assign");
        }
    }

    // --- Phase 3: Register discovered projects ---
    if !args.dry_run {
        let mut project_set: std::collections::HashSet<String> = std::collections::HashSet::new();
        for (_, p) in &classifications {
            if p != "global" {
                project_set.insert(p.clone());
            }
        }
        for (_, p) in already_done.iter().filter_map(|e| e.project.as_ref().map(|p| (&e.id, p))) {
            project_set.insert(p.clone());
        }

        for name in &project_set {
            sqlx::query(
                "INSERT INTO yggdrasil.projects (name, display_name) \
                 VALUES ($1, $1) \
                 ON CONFLICT (name) DO NOTHING",
            )
            .bind(name)
            .execute(&pool)
            .await?;
        }
        tracing::info!(projects = ?project_set, "project registry updated");
    }

    // --- Phase 4: Batch re-upsert into yggdrasil_v2_sdr ---
    tracing::info!("re-upserting into v2 Qdrant collection");

    // Build a map of id → project for all engrams
    let mut project_map: HashMap<Uuid, String> = HashMap::new();
    for (id, proj) in &classifications {
        project_map.insert(*id, proj.clone());
    }
    for e in &already_done {
        if let Some(ref p) = e.project {
            project_map.insert(e.id, p.clone());
        }
    }

    // Re-fetch all engrams with SDR bits for upsert
    let all_sdr_rows: Vec<_> = sqlx::query(
        "SELECT id, sdr_bits, project FROM yggdrasil.engrams \
         WHERE sdr_bits IS NOT NULL",
    )
    .fetch_all(&pool)
    .await?;

    tracing::info!(rows = all_sdr_rows.len(), "SDR rows to upsert");

    let mut batch: Vec<ygg_store::qdrant::PointStruct> = Vec::with_capacity(QDRANT_BATCH_SIZE);
    let mut upserted = 0usize;

    for row in &all_sdr_rows {
        let id: Uuid = row.get("id");
        let sdr_bytes: Vec<u8> = row.get("sdr_bits");

        if sdr_bytes.len() < sdr::SDR_WORDS * 8 {
            tracing::warn!(id = %id, len = sdr_bytes.len(), "skipping invalid SDR");
            continue;
        }

        let sdr_val = sdr::from_bytes(&sdr_bytes);
        let sdr_f32 = sdr::to_bipolar_f32(&sdr_val);

        let project = project_map.get(&id).map(|s| s.as_str());
        let (payload_project, scope) = match project {
            Some("global") | None => (None, "global"),
            Some(p) => (Some(p), "project"),
        };

        let mut payload = HashMap::new();
        if let Some(p) = payload_project {
            payload.insert("project".to_string(), Value::from(p.to_string()));
        }
        payload.insert("scope".to_string(), Value::from(scope.to_string()));

        batch.push(ygg_store::qdrant::PointStruct::new(
            id.to_string(),
            sdr_f32,
            payload,
        ));

        if batch.len() >= QDRANT_BATCH_SIZE {
            if !args.dry_run {
                vectors.upsert_batch(V2_SDR_COLLECTION, std::mem::take(&mut batch)).await?;
            } else {
                batch.clear();
            }
            upserted += QDRANT_BATCH_SIZE;
            tracing::info!(upserted, total = all_sdr_rows.len(), "upsert progress");
        }
    }

    // Flush remaining
    if !batch.is_empty() {
        let remaining = batch.len();
        if !args.dry_run {
            vectors.upsert_batch(V2_SDR_COLLECTION, batch).await?;
        }
        upserted += remaining;
    }

    tracing::info!(upserted, "v2 Qdrant upsert complete");

    // --- Summary ---
    println!("\n=== Migration Summary ===");
    println!("Total engrams:       {total}");
    println!("Already classified:  {}", already_done.len());
    println!("Newly classified:    {}", classifications.len());
    println!("Upserted to v2:     {upserted}");
    if args.dry_run {
        println!("MODE: DRY RUN (no writes performed)");
    }

    Ok(())
}

/// Parse LLM classification response like "1: yggdrasil\n2: global\n3: fenrir"
fn parse_classifications(response: &str, chunk: &[EngramRow]) -> Vec<(Uuid, String)> {
    let mut results = Vec::new();
    let valid_projects = ["yggdrasil", "fenrir", "global"];

    for line in response.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        // Parse "N: project" or "N. project" or "N - project"
        let parts: Vec<&str> = line.splitn(2, |c: char| c == ':' || c == '.' || c == '-').collect();
        if parts.len() != 2 {
            continue;
        }

        let idx: usize = match parts[0].trim().parse::<usize>() {
            Ok(n) if n >= 1 && n <= chunk.len() => n - 1,
            _ => continue,
        };

        let project = parts[1].trim().to_lowercase();
        let project = project.trim_matches(|c: char| !c.is_alphanumeric() && c != '_');

        let assigned = if valid_projects.contains(&project) {
            project.to_string()
        } else {
            "global".to_string()
        };

        results.push((chunk[idx].id, assigned));
    }

    // Any entries not parsed get assigned to global
    for (i, e) in chunk.iter().enumerate() {
        if !results.iter().any(|(id, _)| *id == e.id) {
            tracing::warn!(
                id = %e.id,
                index = i + 1,
                "LLM did not classify entry, defaulting to global"
            );
            results.push((e.id, "global".to_string()));
        }
    }

    results
}
