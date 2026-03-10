# Sprint: 003 - Huginn MVP (Knowledge Indexer)
## Status: IMPLEMENTED

## Objective

Build the Huginn knowledge indexer as a CLI daemon that parses source code files using tree-sitter into semantic AST chunks (functions, structs, enums, impls, traits, modules), generates 1024-dim embeddings for each chunk via Ollama qwen3-embedding, and stores the results in PostgreSQL (metadata + BM25 tsvector) and Qdrant (vector embeddings). Huginn supports two modes: one-shot `index` for bulk indexing of configured directory trees, and `watch` for continuous file monitoring with debounced re-indexing. Incremental indexing is achieved through SHA-256 file content hashing -- only files whose hash has changed since last index are re-processed. Existing chunks for a changed file are deleted and re-inserted (full-file replace, not diff-based patching).

## Scope

### In Scope
- CLI binary with `index` and `watch` subcommands (skeleton already exists in `crates/huginn/src/main.rs`)
- Tree-sitter AST parsing for Rust, Go, Python, TypeScript, JavaScript, and Markdown
- Semantic chunking: extract functions, structs, enums, impls, traits, and module-level doc comments as individual `CodeChunk` entities
- Parent context injection: each chunk includes its enclosing impl/trait/class signature for retrieval disambiguation
- SHA-256 file content hashing for incremental change detection via `yggdrasil.indexed_files` table
- Batch embedding: chunks sent in batches of 10 to Ollama `/api/embed` for throughput
- Dual-write: chunks go to PostgreSQL (`yggdrasil.code_chunks`) and Qdrant (`code_chunks` collection)
- File watcher using `notify` crate with 500ms debounce for real-time re-indexing
- Recursive directory walking with extension-based language detection
- Gitignore-aware file filtering (skip `.git/`, `target/`, `node_modules/`, `__pycache__/`, `vendor/`, `.venv/`)
- YAML config loading via existing `HuginnConfig` struct in `ygg_domain::config`
- Structured tracing for all indexing operations
- Graceful shutdown on SIGTERM/SIGINT in watch mode

### Out of Scope
- Diff-based incremental chunk updates (entire file is re-indexed on change)
- Cross-file dependency analysis or call graph construction
- AST chunking for languages not listed (YAML uses `ChunkType::Config` but is not tree-sitter parsed -- treat each YAML file as a single `Config` chunk)
- HTTP API (Huginn is a CLI daemon, not a server -- Muninn handles retrieval)
- Authentication / authorization
- Prometheus metrics endpoint
- GPU-accelerated parsing
- Multi-repository management (each `index` invocation targets one repo root)

## Hardware Constraints & Utilization Strategy

- **Workload Classification:** Mixed (CPU-bound for tree-sitter parsing; I/O-bound for embedding calls and database writes)
- **Target Hardware:** Hugin (REDACTED_HUGIN_IP) -- AMD Ryzen 7 255 (Zen 5, 8C/16T), 64GB DDR5, AVX-512 support. No discrete GPU.
- **Backend Services:**
  - Hades PostgreSQL: `postgres://jhernandez:K6m4B129CF9u@REDACTED_HADES_IP:5432/postgres` (Intel N150, 32GB RAM, SATA SSD pool "Merlin")
  - Hades Qdrant: `http://REDACTED_HADES_IP:6334` (gRPC, same host as PostgreSQL)
  - Hugin local Ollama: `http://localhost:11434` (qwen3-embedding model, runs on Hugin iGPU / CPU fallback)
- **Utilization Plan:**
  - **Parsing parallelism:** Tree-sitter parsing is CPU-bound and single-threaded per parse call. Use `tokio::task::spawn_blocking` to offload parsing into the blocking thread pool. The Ryzen 7 255 has 16 hardware threads; Tokio's default blocking pool scales to 512 threads, but real parallelism is bounded by the 16 threads. Parse up to 8 files concurrently (half the thread count, leaving headroom for embedding I/O and OS tasks). Use a `tokio::sync::Semaphore` with 8 permits to throttle concurrent parse tasks.
  - **Embedding pipeline:** Ollama embedding is I/O-bound (HTTP call to local service). Batch 10 chunks per request to amortize HTTP overhead. Pipeline: while batch N is embedding, batch N+1 is being parsed. Use a bounded `tokio::sync::mpsc` channel (capacity 4 batches = 40 chunks) between the parser and the embedder to decouple the two stages.
  - **Database writes:** Batched inserts are not supported by the existing `insert_chunk()` function (single-row INSERT). Use concurrent `tokio::spawn` tasks for DB writes, throttled by a separate Semaphore(16) to avoid overwhelming the N150 backend. Qdrant upserts are similarly concurrent.
  - **SQLx connection pool:** 8 connections (matches parsing semaphore -- each parsed file triggers a DB read for hash check and potentially a delete+insert cycle).
  - **AVX-512 opportunity:** SHA-256 hashing of file content can benefit from AVX-512 through the `sha2` crate's auto-vectorization. No code changes needed -- the `sha2` crate detects CPU features at runtime via `cpuid`. Ensure release builds use `target-cpu=native` for optimal codegen.
  - **Memory:** Tree-sitter parsers are lightweight (~2MB per language grammar loaded). With 6 grammars + parsed trees + chunk buffers, expect ~50MB baseline. The 64GB RAM on Hugin is not a constraint.
- **Fallback Strategy:** All parallelism is via Tokio and standard library concurrency primitives. On a lesser machine (e.g., 2-core, no AVX-512), the semaphore permits naturally reduce concurrency to match available cores. SHA-256 falls back to scalar code. The only behavioral difference is throughput -- correctness is hardware-independent.

## Performance Targets

| Metric | Target | Measurement Method |
|--------|--------|--------------------|
| Indexing throughput | > 50 files/minute | Wall-clock time from `index` start to completion, divided by file count, logged at INFO level |
| Parsing throughput (per file) | < 50ms P95 for files under 1000 lines | `tracing` span timing on `parse_file()` |
| Embedding throughput | > 5 chunks/second sustained | Timed across batch embedding calls, logged per batch |
| File watcher latency | < 2 seconds from file save to chunk indexed in both PG and Qdrant | `tracing` span from `notify` event receipt to final DB write completion |
| Debounce effectiveness | Multiple rapid saves to same file produce exactly 1 re-index | Verified by watching a file and saving 5 times in 200ms; assert only 1 indexing pass occurs |
| Memory ceiling (indexing) | < 200MB RSS during bulk index of 1000-file repo | Measured via `/proc/self/status` VmRSS at peak |
| Memory ceiling (watch idle) | < 80MB RSS | Measured after initial index, during idle watch |
| Startup time | < 1s to begin indexing (excludes actual indexing work) | Wall clock from process start to first file parse |

## Data Schemas

### PostgreSQL Tables (Already Exist -- Migration 002)

**`yggdrasil.indexed_files`**
| Column | Type | Constraints |
|--------|------|-------------|
| `file_path` | `TEXT` | PRIMARY KEY |
| `content_hash` | `BYTEA` | NOT NULL (SHA-256 of full file content) |
| `language` | `TEXT` | NOT NULL |
| `chunk_count` | `INTEGER` | NOT NULL, DEFAULT 0 |
| `indexed_at` | `TIMESTAMPTZ` | DEFAULT NOW() |

**`yggdrasil.code_chunks`**
| Column | Type | Constraints |
|--------|------|-------------|
| `id` | `UUID` | PRIMARY KEY, DEFAULT gen_random_uuid() |
| `file_path` | `TEXT` | NOT NULL, FK to `indexed_files(file_path)` ON DELETE CASCADE |
| `repo_root` | `TEXT` | NOT NULL |
| `language` | `TEXT` | NOT NULL |
| `chunk_type` | `TEXT` | NOT NULL (function, struct, enum, impl, trait, module, documentation, config) |
| `name` | `TEXT` | NOT NULL |
| `parent_context` | `TEXT` | DEFAULT '' |
| `content` | `TEXT` | NOT NULL |
| `start_line` | `INTEGER` | NOT NULL |
| `end_line` | `INTEGER` | NOT NULL |
| `content_hash` | `BYTEA` | NOT NULL (SHA-256 of chunk content, used for per-chunk change detection logging) |
| `indexed_at` | `TIMESTAMPTZ` | DEFAULT NOW() |
| `search_vec` | `tsvector` | GENERATED ALWAYS AS (to_tsvector('english', name \|\| ' ' \|\| coalesce(parent_context, '') \|\| ' ' \|\| content)) STORED |

**Indexes (already exist):**
- `idx_chunks_file` on `(file_path)`
- `idx_chunks_language` on `(language)`
- `idx_chunks_type` on `(chunk_type)`
- `idx_chunks_search_vec` GIN on `(search_vec)`

### Qdrant Collection

**Collection name:** `code_chunks`
| Field | Type | Notes |
|-------|------|-------|
| Vector | `Vec<f32>` | 1024 dimensions, Cosine distance |
| Point ID | UUID string | Matches `code_chunks.id` in PostgreSQL |
| Payload | None stored | Metadata retrieved from PostgreSQL by Muninn after Qdrant returns IDs |

### No New Migrations Required
Migration 002 already created both `indexed_files` and `code_chunks` tables with all required columns, indexes, and the generated tsvector column. The `code_chunks` Qdrant collection will be created at runtime via `VectorStore::ensure_collection("code_chunks")`.

## API Contracts

Huginn is a CLI daemon, not an HTTP server. It has no external API. Its interface contracts are:

### CLI Interface

```
huginn index [OPTIONS]
    --config <PATH>     Path to config YAML (default: configs/huginn/config.yaml)
    --repo-root <PATH>  Override repo root (default: from config watch_paths[0])
    --force             Re-index all files regardless of hash match

huginn watch [OPTIONS]
    --config <PATH>     Path to config YAML (default: configs/huginn/config.yaml)
```

### Internal Module Interfaces

**`chunker.rs` -- `Chunker`**
```rust
pub struct Chunker {
    parsers: HashMap<Language, tree_sitter::Parser>,
}

impl Chunker {
    /// Initialize tree-sitter parsers for all supported languages.
    pub fn new() -> Result<Self, HuginnError>;

    /// Parse a source file and extract semantic chunks.
    /// Returns chunks with parent context populated.
    pub fn chunk_file(
        &mut self,
        source: &str,
        language: Language,
        file_path: &str,
        repo_root: &str,
    ) -> Result<Vec<CodeChunk>, HuginnError>;
}
```
Note: `tree_sitter::Parser` is `!Send` and `!Sync`. Each blocking task must create or clone its own `Chunker` instance. Use `thread_local!` or construct per-task.

**`parser.rs` -- Language-Specific Query Patterns**
```rust
/// Returns the tree-sitter Language object for a given Language enum.
pub fn tree_sitter_language(lang: Language) -> Option<tree_sitter::Language>;

/// Returns the tree-sitter query string for extracting semantic nodes.
pub fn extraction_query(lang: Language) -> &'static str;

/// Extract the name from a captured AST node (language-specific logic).
pub fn extract_name(node: tree_sitter::Node, source: &[u8], lang: Language) -> String;

/// Extract parent context string (enclosing impl/trait/class signature).
pub fn extract_parent_context(
    node: tree_sitter::Node,
    source: &[u8],
    lang: Language,
) -> String;

/// Map a tree-sitter node kind string to a ChunkType.
pub fn node_kind_to_chunk_type(kind: &str, lang: Language) -> Option<ChunkType>;
```

**`indexer.rs` -- `Indexer`**
```rust
pub struct Indexer {
    store: ygg_store::Store,
    vectors: ygg_store::qdrant::VectorStore,
    embedder: ygg_embed::EmbedClient,
    config: HuginnConfig,
}

impl Indexer {
    pub async fn new(config: HuginnConfig) -> Result<Self, HuginnError>;

    /// Index all files in configured watch_paths. Returns count of files processed.
    pub async fn index_all(&self, force: bool) -> Result<IndexStats, HuginnError>;

    /// Index a single file. Skips if hash matches unless force=true.
    /// Returns None if skipped, Some(chunk_count) if indexed.
    pub async fn index_file(
        &self,
        path: &Path,
        repo_root: &str,
        force: bool,
    ) -> Result<Option<usize>, HuginnError>;

    /// Delete all chunks for a file (called before re-indexing).
    pub async fn remove_file(&self, path: &str) -> Result<(), HuginnError>;
}

pub struct IndexStats {
    pub files_scanned: usize,
    pub files_indexed: usize,
    pub files_skipped: usize,
    pub chunks_created: usize,
    pub duration: std::time::Duration,
}
```

**`watcher.rs` -- `FileWatcher`**
```rust
pub struct FileWatcher {
    indexer: Arc<Indexer>,
    debounce_ms: u64,
}

impl FileWatcher {
    pub fn new(indexer: Arc<Indexer>, debounce_ms: u64) -> Self;

    /// Start watching configured paths. Blocks until shutdown signal.
    pub async fn run(&self, paths: &[String]) -> Result<(), HuginnError>;
}
```
The watcher uses a `DashMap<PathBuf, Instant>` to track last-seen event time per file. A background task wakes every `debounce_ms` milliseconds, scans the map for entries older than `debounce_ms`, and triggers `index_file()` for each. This ensures rapid successive saves produce exactly one re-index.

**`state.rs` -- Shared State**
```rust
/// Shared runtime state for watch mode.
pub struct AppState {
    pub indexer: Arc<Indexer>,
    pub pending: DashMap<PathBuf, Instant>,
    pub shutdown: tokio::sync::watch::Sender<bool>,
}
```

**`error.rs` -- `HuginnError`**
```rust
pub enum HuginnError {
    Store(ygg_store::error::StoreError),
    Embed(ygg_embed::EmbedError),
    Config(String),
    Parse(String),      // tree-sitter parse failures
    Io(std::io::Error),
    Watch(String),      // notify crate errors
}
```
Implements `From<StoreError>`, `From<EmbedError>`, `From<std::io::Error>`.

## Tree-Sitter Query Patterns

Each language requires a tree-sitter query that captures the semantic AST nodes Huginn extracts. The query uses tree-sitter's S-expression pattern syntax with `@capture` names. The `core-executor` must use these exact queries.

### Rust (`tree-sitter-rust`)

```scheme
;; Functions (free functions and methods)
(function_item
  name: (identifier) @fn_name) @function

;; Struct definitions
(struct_item
  name: (type_identifier) @struct_name) @struct

;; Enum definitions
(enum_item
  name: (type_identifier) @enum_name) @enum

;; Impl blocks
(impl_item
  type: (type_identifier) @impl_type) @impl

;; Trait definitions
(trait_item
  name: (type_identifier) @trait_name) @trait

;; Module declarations (with body)
(mod_item
  name: (identifier) @mod_name) @module
```

**Parent context extraction for Rust:**
- For `function_item` nodes: walk up the AST. If the parent is an `impl_item`, extract the impl signature line (e.g., `impl Foo for Bar`). If the parent is a `trait_item`, extract the trait signature line.
- For `struct_item`, `enum_item`: parent context is the enclosing `mod_item` name, if any.
- Implementation: `node.parent()` traversal, matching on `kind()`.

### Python (`tree-sitter-python`)

```scheme
;; Function definitions (including async def)
(function_definition
  name: (identifier) @fn_name) @function

;; Class definitions
(class_definition
  name: (identifier) @class_name) @struct

;; Decorated definitions (capture the decorator + def together)
(decorated_definition
  definition: (function_definition
    name: (identifier) @fn_name)) @function
```

**Parent context extraction for Python:**
- For `function_definition` inside a `class_definition`: extract the class name and base classes (e.g., `class MyClass(BaseClass):`).
- For nested functions: extract the enclosing function name.

**ChunkType mapping:**
- `function_definition` -> `ChunkType::Function`
- `class_definition` -> `ChunkType::Struct` (Python classes map to Struct in our domain model)
- `decorated_definition` containing a function -> `ChunkType::Function`
- `decorated_definition` containing a class -> `ChunkType::Struct`

### Go (`tree-sitter-go`)

```scheme
;; Function declarations
(function_declaration
  name: (identifier) @fn_name) @function

;; Method declarations (have a receiver)
(method_declaration
  name: (field_identifier) @fn_name) @function

;; Type declarations (struct types)
(type_declaration
  (type_spec
    name: (type_identifier) @struct_name
    type: (struct_type))) @struct

;; Type declarations (interface types)
(type_declaration
  (type_spec
    name: (type_identifier) @trait_name
    type: (interface_type))) @trait
```

**Parent context extraction for Go:**
- For `method_declaration`: extract the receiver type from the receiver parameter list (e.g., `func (s *Server)`). The receiver is the first child matching `parameter_list`.
- For `function_declaration`: no parent context (Go does not have impl blocks; the package name can be used but is not in the AST).

**ChunkType mapping:**
- `function_declaration` -> `ChunkType::Function`
- `method_declaration` -> `ChunkType::Function`
- `struct_type` in `type_declaration` -> `ChunkType::Struct`
- `interface_type` in `type_declaration` -> `ChunkType::Trait`

### TypeScript / JavaScript (`tree-sitter-typescript` / `tree-sitter-javascript`)

```scheme
;; Function declarations
(function_declaration
  name: (identifier) @fn_name) @function

;; Arrow functions assigned to variables
(lexical_declaration
  (variable_declarator
    name: (identifier) @fn_name
    value: (arrow_function))) @function

;; Class declarations
(class_declaration
  name: (identifier) @class_name) @struct

;; Method definitions inside classes
(method_definition
  name: (property_identifier) @fn_name) @function

;; Interface declarations (TypeScript only)
(interface_declaration
  name: (type_identifier) @trait_name) @trait

;; Type alias declarations (TypeScript only)
(type_alias_declaration
  name: (type_identifier) @struct_name) @struct

;; Export statements wrapping functions/classes
(export_statement
  declaration: (function_declaration
    name: (identifier) @fn_name)) @function

(export_statement
  declaration: (class_declaration
    name: (identifier) @class_name)) @struct
```

**Parent context extraction for TypeScript/JavaScript:**
- For `method_definition`: extract the enclosing `class_declaration` name.
- For `arrow_function` in `lexical_declaration`: if inside a class body or module, extract enclosing scope name.

**ChunkType mapping:**
- `function_declaration`, `arrow_function` in variable, `method_definition` -> `ChunkType::Function`
- `class_declaration` -> `ChunkType::Struct`
- `interface_declaration` -> `ChunkType::Trait`
- `type_alias_declaration` -> `ChunkType::Struct`

**Grammar crate note:** TypeScript uses `tree-sitter-typescript` which provides two languages: `language_typescript()` and `language_tsx()`. Use `language_typescript()` for `.ts` files and `language_tsx()` for `.tsx` files. JavaScript uses `tree-sitter-javascript`.

### Markdown (`tree-sitter-md`)

Markdown is not chunked by AST node types. Instead, treat each top-level heading section (from one `#`/`##` heading to the next) as a `ChunkType::Documentation` chunk. The chunk `name` is the heading text.

```scheme
;; Headings (atx_heading covers #, ##, ###, etc.)
(atx_heading
  (heading_content) @heading_text) @heading
```

**Chunking strategy for Markdown:**
- Split the file at each heading node. Each section (heading + body until next heading) becomes one `Documentation` chunk.
- The `name` field is the heading text.
- The `parent_context` is the file name.
- If a file has no headings, treat the entire file as a single `Documentation` chunk with `name` = filename.

## Interface Boundaries

| Module | Owns | Exposes | Depends On |
|--------|------|---------|------------|
| `huginn::main` | Process lifecycle, CLI parsing, config loading, signal handling | Nothing (binary entrypoint) | `indexer`, `watcher`, `error`, `ygg_domain::config` |
| `huginn::chunker` | Tree-sitter parser pool, file-to-chunks transformation | `Chunker` struct with `chunk_file()` | `huginn::parser`, `tree-sitter`, `ygg_domain::chunk` |
| `huginn::parser` | Language grammar loading, query patterns, name/context extraction | `tree_sitter_language()`, `extraction_query()`, `extract_name()`, `extract_parent_context()`, `node_kind_to_chunk_type()` | `tree-sitter-rust`, `tree-sitter-python`, `tree-sitter-go`, `tree-sitter-typescript`, `tree-sitter-javascript`, `tree-sitter-md` |
| `huginn::indexer` | Indexing orchestration: file walking, hash comparison, chunk persistence, embedding pipeline | `Indexer` struct with `index_all()`, `index_file()`, `remove_file()`, `IndexStats` | `huginn::chunker`, `ygg_store::Store`, `ygg_store::qdrant::VectorStore`, `ygg_embed::EmbedClient`, `ygg_store::postgres::chunks::*` |
| `huginn::watcher` | File system event monitoring, debounce logic | `FileWatcher` struct with `run()` | `huginn::indexer`, `notify`, `dashmap`, `tokio::time` |
| `huginn::state` | Shared runtime state for watch mode | `AppState` struct | `huginn::indexer`, `dashmap` |
| `huginn::error` | Error type unification | `HuginnError` enum, `From` impls | `ygg_store::error::StoreError`, `ygg_embed::EmbedError` |
| `ygg_store` (external) | PostgreSQL connection pool, chunk/file CRUD, Qdrant client | `Store`, `postgres::chunks::*`, `qdrant::VectorStore` | `sqlx`, `qdrant-client`, `ygg_domain` |
| `ygg_embed` (external) | Ollama HTTP communication | `EmbedClient`, `embed_batch()` | `reqwest` |
| `ygg_domain` (external) | Type definitions, config structs | `chunk::*`, `config::HuginnConfig` | None (leaf crate) |

**Ownership rules:**
- Only `huginn::indexer` may call `ygg_store::postgres::chunks::*` functions. No other huginn module touches SQL.
- Only `huginn::indexer` may call `ygg_embed::EmbedClient`. No other huginn module triggers embedding.
- Only `huginn::chunker` may call tree-sitter parsing APIs. The indexer receives parsed `Vec<CodeChunk>` results.
- Only `huginn::parser` may load tree-sitter language grammars. The chunker receives `tree_sitter::Language` objects from parser.
- `huginn::watcher` delegates all indexing to `huginn::indexer`. It never touches the database or embedding service directly.

## File-Level Implementation Plan

### `crates/huginn/Cargo.toml` (MODIFY)

Add tree-sitter grammar crates. These are NOT in the workspace `[workspace.dependencies]` since only Huginn uses them. Add them as direct dependencies:

```toml
[dependencies]
# ... existing deps ...
tree-sitter-rust = "0.23"
tree-sitter-python = "0.23"
tree-sitter-go = "0.23"
tree-sitter-typescript = "0.23"
tree-sitter-javascript = "0.23"
tree-sitter-md = "0.3"
futures = { workspace = true }
walkdir = "2"
```

**Important version note:** The tree-sitter grammar crates must be compatible with `tree-sitter = "0.24"` (workspace dep). The grammar crates at `0.23.x` expose a `LANGUAGE` static or `language()` function returning a `tree_sitter::Language`. The `core-executor` must verify version compatibility at compile time. If the `0.23` grammar crates are incompatible with tree-sitter 0.24, pin the grammar crates to the version that targets tree-sitter 0.24 (typically the latest minor release). Check each crate's `tree-sitter` dependency in its own `Cargo.toml`.

Also add `walkdir = "2"` to `[workspace.dependencies]` in the root `Cargo.toml` since directory walking is a general utility.

### `crates/huginn/src/error.rs` (NEW)

Define `HuginnError` enum as specified in API Contracts. Variants: `Store`, `Embed`, `Config`, `Parse`, `Io`, `Watch`. Implement `Display` via `thiserror`, `From<StoreError>`, `From<EmbedError>`, `From<std::io::Error>`.

### `crates/huginn/src/parser.rs` (NEW)

1. Define `tree_sitter_language(lang: Language) -> Option<tree_sitter::Language>` using the grammar crates.
2. Define `extraction_query(lang: Language) -> &'static str` returning the query strings from the Tree-Sitter Query Patterns section above.
3. Define `extract_name()` -- for each captured node, extract the name child text from the source bytes.
4. Define `extract_parent_context()` -- walk `node.parent()` up the AST tree, looking for enclosing impl/trait/class nodes. Return their signature line (first line of the node's text).
5. Define `node_kind_to_chunk_type()` -- map the `@capture` name to `ChunkType` (the capture name in the query determines the type: `@function` -> Function, `@struct` -> Struct, etc.).
6. Return `None` from `tree_sitter_language()` for `Language::Yaml` and `Language::Unknown` -- these are not tree-sitter parsed.

### `crates/huginn/src/chunker.rs` (NEW)

1. Define `Chunker` struct holding a `HashMap<Language, tree_sitter::Parser>`.
2. `Chunker::new()` -- for each supported language, create a `tree_sitter::Parser`, call `parser.set_language(tree_sitter_language(lang))`, and store it.
3. `Chunker::chunk_file(source, language, file_path, repo_root)`:
   a. Get the parser for the language (return empty vec for unsupported languages).
   b. Call `parser.parse(source, None)` to get the tree.
   c. Create a `tree_sitter::QueryCursor` with the `extraction_query(language)`.
   d. Iterate `cursor.matches()`. For each match:
      - Determine the capture name to identify which `ChunkType` this is.
      - Extract the full node text from `source` as `content`.
      - Call `extract_name()` for the name field.
      - Call `extract_parent_context()` for the parent context.
      - Compute SHA-256 of `content` for `content_hash`.
      - Build a `CodeChunk` with a new `Uuid::new_v4()`, file_path, repo_root, language, chunk_type, name, parent_context, content, start_line (node.start_position().row + 1), end_line (node.end_position().row + 1), content_hash, and `Utc::now()` for indexed_at.
   e. Return the collected `Vec<CodeChunk>`.
4. **Special handling for Markdown:** If language is Markdown, use the heading-section splitting strategy described in the query patterns section instead of the standard query-cursor approach.
5. **Special handling for YAML:** If language is Yaml, skip tree-sitter entirely. Create a single `CodeChunk` with `ChunkType::Config`, `name` = filename, and `content` = entire file content.

### `crates/huginn/src/indexer.rs` (NEW)

1. Define `Indexer` struct and `IndexStats` as specified in API Contracts.
2. `Indexer::new(config)`:
   a. `Store::connect(&config.database_url).await`
   b. `Store::migrate("./migrations").await` -- runs migrations 001 and 002 (idempotent)
   c. `VectorStore::connect(&config.qdrant_url).await`
   d. `VectorStore::ensure_collection("code_chunks").await`
   e. `EmbedClient::new(&config.embed.ollama_url, &config.embed.model)`
3. `Indexer::index_all(force)`:
   a. Walk each directory in `config.watch_paths` using `walkdir::WalkDir`.
   b. Filter files by extension using `Language::from_extension()`. Skip `Language::Unknown`.
   c. Apply ignore rules: skip paths containing `/.git/`, `/target/`, `/node_modules/`, `/__pycache__/`, `/vendor/`, `/.venv/`.
   d. For each file, call `self.index_file(path, repo_root, force).await`.
   e. Use `tokio::sync::Semaphore(8)` to limit concurrent file indexing.
   f. Collect `IndexStats` and log summary at INFO level.
4. `Indexer::index_file(path, repo_root, force)`:
   a. Read file content via `tokio::fs::read_to_string(path).await`.
   b. Compute SHA-256 of file content.
   c. Call `ygg_store::postgres::chunks::get_indexed_file(pool, file_path).await`.
   d. If hash matches and `!force`, return `None` (skip).
   e. If file was previously indexed (hash differs or force): call `self.remove_file(file_path).await` to delete old chunks.
   f. Determine `Language` from file extension.
   g. Spawn a blocking task for tree-sitter parsing: `tokio::task::spawn_blocking(move || { Chunker::new()?.chunk_file(...) })`.
   h. Batch the resulting chunks into groups of 10.
   i. For each batch, call `self.embedder.embed_batch(&texts).await` where `texts` is the chunk `content` fields.
   j. For each chunk + embedding pair:
      - Call `ygg_store::postgres::chunks::insert_chunk(pool, &chunk).await`.
      - Call `self.vectors.upsert("code_chunks", chunk.id, embedding, HashMap::new()).await`.
   k. Call `ygg_store::postgres::chunks::upsert_indexed_file(pool, file_path, &hash, language, chunk_count).await`.
   l. Return `Some(chunk_count)`.
5. `Indexer::remove_file(file_path)`:
   a. Query Qdrant for all point IDs that have this file_path. Since Qdrant stores no payload, we must get the chunk IDs from PostgreSQL first.
   b. Query PostgreSQL: `SELECT id FROM yggdrasil.code_chunks WHERE file_path = $1`.
   c. For each ID, call `self.vectors.delete("code_chunks", id).await`.
   d. Call `ygg_store::postgres::chunks::delete_chunks_for_file(pool, file_path).await` -- CASCADE from indexed_files will also clean up, but explicit delete ensures Qdrant is cleaned first.

**Note on ygg_store additions:** The `remove_file` flow requires a new query function in `ygg_store::postgres::chunks`:
```rust
pub async fn get_chunk_ids_for_file(
    pool: &PgPool,
    file_path: &str,
) -> Result<Vec<Uuid>, StoreError>;
```
This function runs `SELECT id FROM yggdrasil.code_chunks WHERE file_path = $1`. The `core-executor` must add this to `crates/ygg-store/src/postgres/chunks.rs`.

Additionally, `VectorStore` needs a batch delete method for efficiency:
```rust
pub async fn delete_many(
    &self,
    collection: &str,
    ids: &[Uuid],
) -> Result<(), StoreError>;
```
This wraps `DeletePointsBuilder` with a `PointsIdsList` containing all IDs. The `core-executor` must add this to `crates/ygg-store/src/qdrant.rs`.

### `crates/huginn/src/watcher.rs` (NEW)

1. Define `FileWatcher` struct as specified.
2. `FileWatcher::run(paths)`:
   a. Create a `notify::recommended_watcher()` with an `mpsc::channel` event sink.
   b. For each path in `paths`, call `watcher.watch(path, RecursiveMode::Recursive)`.
   c. Spawn a Tokio task that reads from the notify channel:
      - On `Create`, `Modify`, or `Remove` events, extract the file path.
      - Apply the same ignore rules as `index_all` (skip `.git/`, `target/`, etc.).
      - Check file extension via `Language::from_extension()`. Skip Unknown.
      - Insert/update `pending.insert(path, Instant::now())` in the shared `DashMap`.
   d. Spawn a debounce tick task (`tokio::time::interval(Duration::from_millis(debounce_ms))`):
      - On each tick, iterate the `pending` DashMap.
      - For entries where `Instant::now() - entry_time >= debounce_duration`:
        - Remove the entry from the map.
        - If the file still exists: call `indexer.index_file(path, repo_root, false).await`.
        - If the file was deleted: call `indexer.remove_file(path).await`.
   e. Wait for shutdown signal (`tokio::signal::ctrl_c()` or `watch::Receiver` from `AppState`).
   f. Log "watcher shutting down" and return.

### `crates/huginn/src/state.rs` (NEW)

Define `AppState` as specified. This is primarily a data holder for watch mode, grouping the `Indexer`, pending events `DashMap`, and shutdown channel.

### `crates/huginn/src/main.rs` (MODIFY -- replace skeleton)

1. Parse CLI args. Expand the existing `Command` enum to include `--force` flag on `Index` and `--repo-root` override.
2. Load `HuginnConfig` from YAML file at `cli.config` path via `serde_yaml::from_reader()`.
3. Construct `Indexer::new(config.clone()).await`.
4. Match on command:
   - `Command::Index { force, repo_root }`:
     - If `repo_root` is provided, override `config.watch_paths` with `vec![repo_root]`.
     - Call `indexer.index_all(force).await`.
     - Log `IndexStats` summary.
     - Exit 0.
   - `Command::Watch`:
     - Wrap indexer in `Arc`.
     - Create `FileWatcher::new(indexer, config.debounce_ms)`.
     - Call `watcher.run(&config.watch_paths).await`.
     - Exit 0 on shutdown signal.

### `configs/huginn/config.yaml` (NEW)

```yaml
watch_paths:
  - "/home/jesus/Documents/HardwareSetup/yggdrasil"
  - "/home/jesus/Documents/Rust/Fergus_Agent/fergus-rs"
database_url: "postgres://jhernandez:K6m4B129CF9u@REDACTED_HADES_IP:5432/postgres"
qdrant_url: "http://REDACTED_HADES_IP:6334"
embed:
  ollama_url: "http://localhost:11434"
  model: "qwen3-embedding"
debounce_ms: 500
```

## Embedding Strategy

Each `CodeChunk` is embedded as a single text constructed by concatenating context and content:

```
{language} {chunk_type}: {name}
Parent: {parent_context}
{content}
```

Example for a Rust function:
```
rust function: handle_completion
Parent: impl Orchestrator for ServerState
pub async fn handle_completion(&self, req: CompletionRequest) -> Result<Response> {
    let context = self.build_context(&req).await?;
    ...
}
```

This format ensures the embedding captures both the semantic identity (what it is, where it lives) and the implementation details (what it does). The `language` and `chunk_type` prefixes help the embedding model distinguish between, say, a Python `class Foo` and a Rust `struct Foo`.

The text is passed to `EmbedClient::embed_batch()` via the Ollama `/api/embed` endpoint. Batch size is 10 chunks per request. If a batch fails, retry once after a 1-second delay. If the retry fails, log the error and skip the batch (do not abort the entire indexing run).

## Acceptance Criteria

- [ ] `cargo build --release -p huginn` compiles with zero errors and zero warnings
- [ ] `huginn index --config configs/huginn/config.yaml` successfully indexes the yggdrasil workspace
- [ ] Indexed files appear in `yggdrasil.indexed_files` with correct SHA-256 hashes and chunk counts
- [ ] Code chunks appear in `yggdrasil.code_chunks` with correct language, chunk_type, name, parent_context, content, line numbers
- [ ] Code chunk embeddings appear in Qdrant `code_chunks` collection with matching UUIDs
- [ ] Running `huginn index` a second time without file changes results in 0 files re-indexed (all skipped by hash match)
- [ ] Modifying a single file and running `huginn index` re-indexes only that file (old chunks deleted, new chunks inserted)
- [ ] `huginn index --force` re-indexes all files regardless of hash match
- [ ] `huginn watch` detects file saves and re-indexes within 2 seconds
- [ ] Rapid successive saves to the same file (5 saves in 200ms) produce exactly 1 re-index pass
- [ ] File deletion in watch mode removes the file's chunks from both PostgreSQL and Qdrant
- [ ] Rust files produce chunks of type: Function, Struct, Enum, Impl, Trait, Module
- [ ] Python files produce chunks of type: Function, Struct (for classes)
- [ ] Go files produce chunks of type: Function, Struct, Trait (for interfaces)
- [ ] TypeScript files produce chunks of type: Function, Struct (for classes/types), Trait (for interfaces)
- [ ] Markdown files produce chunks of type: Documentation (one per heading section)
- [ ] YAML files produce a single Config chunk per file
- [ ] Parent context is correctly populated (e.g., a method inside `impl Foo` has parent_context = "impl Foo")
- [ ] Embedding text format matches the Embedding Strategy section (language + type + name + parent + content)
- [ ] Batch embedding sends chunks in groups of 10 to Ollama
- [ ] Indexing throughput exceeds 50 files/minute on the yggdrasil workspace
- [ ] Memory stays below 200MB RSS during bulk indexing
- [ ] Directories matching ignore patterns (.git, target, node_modules, __pycache__, vendor, .venv) are skipped
- [ ] Config loads from `configs/huginn/config.yaml` by default, overridable via `--config` flag
- [ ] Graceful shutdown on SIGTERM/SIGINT in watch mode -- in-flight indexing completes before exit
- [ ] tsvector `search_vec` column is automatically populated for each chunk (verified by querying `SELECT search_vec IS NOT NULL FROM yggdrasil.code_chunks`)

## Dependencies

| Dependency | Type | Status | Blocking? |
|------------|------|--------|-----------|
| Sprint 001 (Foundation) | Sprint | DONE | No |
| Sprint 002 (Mimir MVP) | Sprint | DONE | No |
| Migration 002 (index metadata) | Database | DONE -- tables exist on Hades | No |
| Hades PostgreSQL | Infrastructure | Running | Yes -- required at runtime |
| Hades Qdrant | Infrastructure | Status uncertain | Yes -- must be verified by `infra-devops` |
| Hugin Ollama (qwen3-embedding) | Infrastructure | Must be verified | Yes -- `infra-devops` must ensure model is pulled on Hugin |
| `ygg_domain` crate | Code | Complete | No |
| `ygg_store` crate | Code | Complete (needs 2 new functions, see Implementation Plan) | No |
| `ygg_embed` crate | Code | Complete | No |
| `tree-sitter` 0.24 workspace dep | Code | Available | No |
| `tree-sitter-rust`, `tree-sitter-python`, `tree-sitter-go`, `tree-sitter-typescript`, `tree-sitter-javascript` | Code | Must be added to huginn Cargo.toml | No |
| `tree-sitter-md` | Code | Must be added to huginn Cargo.toml | No |
| `walkdir` | Code | Must be added to workspace deps and huginn Cargo.toml | No |
| `notify` 7 workspace dep | Code | Available | No |

## Risks & Mitigations

| Risk | Impact | Likelihood | Mitigation |
|------|--------|------------|------------|
| Tree-sitter grammar crate versions incompatible with tree-sitter 0.24 | Compilation failure | Medium | `core-executor` must check each grammar crate's tree-sitter dependency version. If 0.23 grammar crates require tree-sitter 0.23, either downgrade workspace tree-sitter to 0.23 or find grammar crate versions that target 0.24. Document the working version combination. |
| Qdrant on Hades not yet configured | Huginn cannot store embeddings | Medium (NetworkHardware.md says "no idea if it's configured yet") | `infra-devops` must verify Qdrant is running and reachable at `http://REDACTED_HADES_IP:6334` before `core-executor` integration tests. |
| Ollama qwen3-embedding model not pulled on Hugin | Embedding calls fail with 404 | Medium (Hugin is listed as "Experimental AI", model presence not confirmed) | `infra-devops` must run `ollama pull qwen3-embedding` on Hugin (REDACTED_HUGIN_IP). |
| tree_sitter::Parser is !Send + !Sync | Cannot share Parser across Tokio tasks | High (confirmed API constraint) | Create a new `Chunker` instance inside each `spawn_blocking` closure. Parsers are cheap to construct (~5ms). Do not try to share them. |
| Large files cause OOM during embedding | Ollama may reject or timeout on very large chunks | Low | Cap chunk content at 8192 characters. If a chunk exceeds this, truncate to 8192 chars for the embedding text but store the full content in PostgreSQL. Log a warning. |
| File rename detected as delete + create | Double work: old file chunks deleted, new file re-indexed | Low | Acceptable for MVP. The `notify` crate emits separate Remove and Create events for renames. Both are handled correctly (remove cleans old path, create indexes new path). |
| Concurrent watch events for many files overwhelm Hades N150 | Database connection pool exhaustion, slow inserts | Medium (during active development with auto-save) | The 500ms debounce + Semaphore(8) limit naturally throttle. If needed, increase debounce to 1000ms via config. |
| SHA-256 file hash collision | A changed file is skipped | Negligible (2^-256) | Not mitigated. Accept the theoretical risk. |

## Decision Log

| Date | Decision | Rationale |
|------|----------|-----------|
| 2026-03-09 | Use tree-sitter for AST chunking, not regex or line-based splitting | Tree-sitter produces a full syntax tree, enabling extraction of complete semantic units (functions, structs) with their exact boundaries. Regex-based chunking is fragile across languages and cannot handle nested structures. |
| 2026-03-09 | Full-file re-index on change, not diff-based chunk patching | Diff-based patching requires tracking which chunks correspond to which AST nodes across edits, adding significant complexity for minimal benefit. A full-file re-index takes <100ms for typical source files and is simpler to reason about. |
| 2026-03-09 | Construct new Chunker per spawn_blocking task, not shared | tree_sitter::Parser is !Send and !Sync. Sharing across tasks would require unsafe code or a Mutex that serializes all parsing. Per-task construction is safe, simple, and cheap (~5ms per Chunker with 6 languages). |
| 2026-03-09 | Batch embeddings in groups of 10 | Balances HTTP overhead (fewer requests) against latency (smaller batches complete faster). 10 chunks is ~2-5KB of text, well within Ollama's request size limits. Allows pipeline overlap with parsing. |
| 2026-03-09 | Qdrant stores no payload (metadata in PostgreSQL only) | Consistent with Mimir's pattern. Single source of truth for metadata in PostgreSQL. Qdrant is solely a vector index. Muninn (retrieval) will query Qdrant for IDs, then enrich from PostgreSQL. |
| 2026-03-09 | Embed chunk with language/type/name prefix, not raw content alone | The prefix gives the embedding model semantic context that raw code may not convey. Distinguishes "a Python class named Foo" from "a Go struct named Foo" in the vector space, improving retrieval relevance. |
| 2026-03-09 | Map Python class to ChunkType::Struct, Go interface to ChunkType::Trait | The domain model uses Rust-centric terminology. Rather than adding language-specific ChunkType variants (Class, Interface), map to the closest Rust equivalent. This keeps the enum small and the query interface uniform. |
| 2026-03-09 | YAML files treated as single Config chunk, no tree-sitter parsing | YAML files are configuration, not code. Semantic chunking adds no value. A single chunk per file with full content is sufficient for retrieval. |
| 2026-03-09 | Cap embedding text at 8192 characters | qwen3-embedding has a context window. Extremely long functions would produce poor embeddings if truncated internally by the model with no warning. Explicit truncation with a logged warning makes the behavior visible. |
| 2026-03-09 | Use walkdir crate for directory traversal, not std::fs::read_dir recursion | walkdir handles symlinks, permissions errors, and recursion depth limits out of the box. Avoids reinventing directory walking. |
| 2026-03-09 | Add get_chunk_ids_for_file() and VectorStore::delete_many() to ygg_store | Huginn needs to clean Qdrant when re-indexing a file. The existing delete_chunks_for_file() only handles PostgreSQL. These additions keep the Qdrant cleanup inside the store layer where it belongs, not in Huginn. |

---

**Next agent:** `core-executor` -- implement all files listed in the File-Level Implementation Plan. Execution order:
1. Add `walkdir = "2"` to `[workspace.dependencies]` in root `Cargo.toml`.
2. Update `crates/huginn/Cargo.toml` with tree-sitter grammar crates, `walkdir`, and `futures`.
3. Add `get_chunk_ids_for_file()` to `crates/ygg-store/src/postgres/chunks.rs`.
4. Add `delete_many()` to `crates/ygg-store/src/qdrant.rs`.
5. Create `crates/huginn/src/error.rs`.
6. Create `crates/huginn/src/parser.rs`.
7. Create `crates/huginn/src/chunker.rs`.
8. Create `crates/huginn/src/state.rs`.
9. Create `crates/huginn/src/indexer.rs`.
10. Create `crates/huginn/src/watcher.rs`.
11. Replace `crates/huginn/src/main.rs`.
12. Create `configs/huginn/config.yaml`.
13. Verify compilation: `cargo build --release -p huginn`.

**Blocker check for `infra-devops`:** Before `core-executor` can integration-test, `infra-devops` must verify:
1. Qdrant is running and reachable at `http://REDACTED_HADES_IP:6334` from Hugin
2. `qwen3-embedding` model is pulled on Hugin (REDACTED_HUGIN_IP): `ollama list | grep qwen3-embedding`
3. PostgreSQL on Hades accepts connections from Hugin's IP (REDACTED_HUGIN_IP)
