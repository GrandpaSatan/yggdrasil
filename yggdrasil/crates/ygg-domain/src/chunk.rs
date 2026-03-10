use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Supported programming languages for code chunking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Language {
    Rust,
    Go,
    Python,
    TypeScript,
    JavaScript,
    Markdown,
    Yaml,
    Unknown,
}

impl Language {
    pub fn from_extension(ext: &str) -> Self {
        match ext {
            "rs" => Self::Rust,
            "go" => Self::Go,
            "py" => Self::Python,
            "ts" | "tsx" => Self::TypeScript,
            "js" | "jsx" => Self::JavaScript,
            "md" | "mdx" => Self::Markdown,
            "yaml" | "yml" => Self::Yaml,
            _ => Self::Unknown,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::Go => "go",
            Self::Python => "python",
            Self::TypeScript => "typescript",
            Self::JavaScript => "javascript",
            Self::Markdown => "markdown",
            Self::Yaml => "yaml",
            Self::Unknown => "unknown",
        }
    }
}

impl std::fmt::Display for Language {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Semantic unit type extracted from AST.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChunkType {
    Function,
    Struct,
    Enum,
    Impl,
    Trait,
    Module,
    Documentation,
    Config,
}

impl ChunkType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Function => "function",
            Self::Struct => "struct",
            Self::Enum => "enum",
            Self::Impl => "impl",
            Self::Trait => "trait",
            Self::Module => "module",
            Self::Documentation => "documentation",
            Self::Config => "config",
        }
    }
}

impl std::fmt::Display for ChunkType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A semantic code chunk extracted by tree-sitter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeChunk {
    pub id: Uuid,
    pub file_path: String,
    pub repo_root: String,
    pub language: Language,
    pub chunk_type: ChunkType,
    /// e.g. "fn handle_completion"
    pub name: String,
    /// e.g. "impl Orchestrator for ServerState"
    pub parent_context: String,
    pub content: String,
    pub start_line: usize,
    pub end_line: usize,
    /// SHA-256 of content for change detection.
    pub content_hash: Vec<u8>,
    pub indexed_at: DateTime<Utc>,
}

/// Metadata for a tracked file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexedFile {
    pub file_path: String,
    pub content_hash: Vec<u8>,
    pub language: Language,
    pub chunk_count: i32,
    pub indexed_at: DateTime<Utc>,
}

/// A search result from the retrieval engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub chunk: CodeChunk,
    pub score: f64,
    pub source: SearchSource,
}

/// Which search modality produced this result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchSource {
    Vector,
    Bm25,
    Fused,
}

/// Search query parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchQuery {
    pub query: String,
    #[serde(default = "default_search_limit")]
    pub limit: usize,
    #[serde(default)]
    pub languages: Option<Vec<Language>>,
}

fn default_search_limit() -> usize {
    10
}

/// Search response with assembled context.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResponse {
    pub results: Vec<SearchResult>,
    pub context: String,
}
