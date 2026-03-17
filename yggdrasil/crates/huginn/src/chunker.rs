use chrono::Utc;
use sha2::{Digest, Sha256};
use streaming_iterator::StreamingIterator as _;
use uuid::Uuid;
use ygg_domain::chunk::{ChunkType, CodeChunk, Language};

use crate::error::HuginnError;
use crate::parser::{
    extraction_query, extract_parent_context, node_kind_to_chunk_type, tree_sitter_language,
};

/// Maximum characters allowed in an embedding text payload (sprint 003 §Risks).
/// Chunks whose content exceeds this are truncated for embedding but stored in full
/// in PostgreSQL. A warning is logged at the call site in indexer.rs.
pub const MAX_EMBED_CHARS: usize = 8192;

/// Holds tree-sitter parsers for all supported languages.
///
/// `tree_sitter::Parser` is `!Send + !Sync`. Each `spawn_blocking` task must
/// construct its own `Chunker` — never share across threads or async tasks.
pub struct Chunker {
    // Vec because `Language` does not implement `Hash`. With 6 entries, linear
    // scan has negligible overhead compared to tree-sitter parse time.
    parsers: Vec<(Language, tree_sitter::Parser)>,
}

impl Chunker {
    /// Initialise tree-sitter parsers for all supported languages.
    pub fn new() -> Result<Self, HuginnError> {
        let languages = [
            Language::Rust,
            Language::Python,
            Language::Go,
            Language::JavaScript,
            Language::TypeScript,
            Language::Markdown,
        ];
        let mut parsers = Vec::with_capacity(languages.len());
        for lang in languages {
            let ts_lang = match tree_sitter_language(lang) {
                Some(l) => l,
                None => continue,
            };
            let mut parser = tree_sitter::Parser::new();
            parser
                .set_language(&ts_lang)
                .map_err(|e| HuginnError::Parse(format!("set_language({lang}): {e}")))?;
            parsers.push((lang, parser));
        }
        Ok(Self { parsers })
    }

    /// Retrieve a mutable reference to the parser for `lang`, if present.
    fn parser_for(&mut self, lang: Language) -> Option<&mut tree_sitter::Parser> {
        self.parsers
            .iter_mut()
            .find(|(l, _)| *l == lang)
            .map(|(_, p)| p)
    }

    /// Parse a source file and extract semantic chunks.
    ///
    /// - YAML: single `Config` chunk, no tree-sitter.
    /// - Markdown: heading-section splitting strategy.
    /// - All code languages: tree-sitter query cursor extracts named nodes.
    /// - Unknown: returns empty vec.
    pub fn chunk_file(
        &mut self,
        source: &str,
        language: Language,
        file_path: &str,
        repo_root: &str,
    ) -> Result<Vec<CodeChunk>, HuginnError> {
        match language {
            Language::Yaml => {
                return Ok(vec![build_single_chunk(
                    source,
                    language,
                    ChunkType::Config,
                    file_name(file_path),
                    String::new(),
                    file_path,
                    repo_root,
                )]);
            }
            Language::Markdown => {
                return self.chunk_markdown(source, file_path, repo_root);
            }
            Language::Unknown => {
                return Ok(vec![]);
            }
            _ => {}
        }

        let ts_lang = match tree_sitter_language(language) {
            Some(l) => l,
            None => return Ok(vec![]),
        };

        let query_src = extraction_query(language);
        if query_src.is_empty() {
            return Ok(vec![]);
        }

        // Build the query before borrowing the parser mutably.
        let query = tree_sitter::Query::new(&ts_lang, query_src)
            .map_err(|e| HuginnError::Parse(format!("query compile error for {language}: {e}")))?;

        let capture_names: Vec<String> = query
            .capture_names()
            .iter()
            .map(|s| s.to_string())
            .collect();

        let parser = match self.parser_for(language) {
            Some(p) => p,
            None => return Ok(vec![]),
        };

        let tree = parser
            .parse(source.as_bytes(), None)
            .ok_or_else(|| HuginnError::Parse(format!("tree-sitter parse failed: {file_path}")))?;

        let source_bytes = source.as_bytes();
        let mut cursor = tree_sitter::QueryCursor::new();

        // tree-sitter 0.24 uses `StreamingIterator` for QueryMatches.
        // We collect into a Vec first to avoid lifetime conflicts with the mutable parser borrow.
        let mut raw_matches: Vec<(usize, usize, usize)> = Vec::new(); // (cap_idx, start_byte, end_byte)
        let mut stream = cursor.matches(&query, tree.root_node(), source_bytes);
        while let Some(m) = stream.next() {
            for cap in m.captures {
                let name = match capture_names.get(cap.index as usize) {
                    Some(n) => n.as_str(),
                    None => continue,
                };
                if node_kind_to_chunk_type(name).is_some() {
                    raw_matches.push((
                        cap.index as usize,
                        cap.node.start_byte(),
                        cap.node.end_byte(),
                    ));
                }
            }
        }

        // Deduplicate by byte range to avoid double-counting overlapping captures
        // (e.g., `export_statement @function` and bare `function_declaration @function`).
        let mut seen: std::collections::HashSet<(usize, usize)> =
            std::collections::HashSet::new();
        let mut chunks = Vec::new();

        for (cap_idx, start_byte, end_byte) in raw_matches {
            let key = (start_byte, end_byte);
            if !seen.insert(key) {
                continue;
            }

            let capture_name = &capture_names[cap_idx];
            let chunk_type = match node_kind_to_chunk_type(capture_name) {
                Some(ct) => ct,
                None => continue,
            };

            // Re-resolve the node from the tree using byte range for the name/context extraction.
            // We need to descend from root to find the node at this exact byte range.
            let node = match tree
                .root_node()
                .descendant_for_byte_range(start_byte, end_byte)
            {
                Some(n) => n,
                None => continue,
            };

            let content = match node.utf8_text(source_bytes) {
                Ok(t) => t.to_string(),
                Err(_) => continue,
            };

            if content.trim().is_empty() {
                continue;
            }

            let name = extract_node_name(node, source_bytes, &query, &capture_names, start_byte, end_byte);
            let parent_context = extract_parent_context(node, source_bytes, language);

            let start_line = node.start_position().row + 1;
            let end_line = node.end_position().row + 1;
            let content_hash = sha256_bytes(content.as_bytes());

            chunks.push(CodeChunk {
                id: Uuid::new_v4(),
                file_path: file_path.to_string(),
                repo_root: repo_root.to_string(),
                language,
                chunk_type,
                name,
                parent_context,
                content,
                start_line,
                end_line,
                content_hash,
                indexed_at: Utc::now(),
            });
        }

        Ok(chunks)
    }

    /// Heading-section splitting for Markdown files.
    ///
    /// Each `atx_heading` marks the start of a new chunk. The section extends
    /// from the heading line to the line before the next heading (or EOF).
    /// If the file has no headings, the entire file becomes one Documentation chunk.
    fn chunk_markdown(
        &mut self,
        source: &str,
        file_path: &str,
        repo_root: &str,
    ) -> Result<Vec<CodeChunk>, HuginnError> {
        let fallback = || {
            vec![build_single_chunk(
                source,
                Language::Markdown,
                ChunkType::Documentation,
                file_name(file_path),
                file_name(file_path).to_string(),
                file_path,
                repo_root,
            )]
        };

        let parser = match self.parser_for(Language::Markdown) {
            Some(p) => p,
            None => return Ok(fallback()),
        };

        let tree = match parser.parse(source.as_bytes(), None) {
            Some(t) => t,
            None => return Ok(fallback()),
        };

        let ts_lang: tree_sitter::Language = tree_sitter_md::LANGUAGE.into();
        let query_src = "(atx_heading (inline) @heading_text) @heading";
        let query = tree_sitter::Query::new(&ts_lang, query_src)
            .map_err(|e| HuginnError::Parse(format!("markdown query error: {e}")))?;

        let capture_names: Vec<String> = query
            .capture_names()
            .iter()
            .map(|s| s.to_string())
            .collect();

        let source_bytes = source.as_bytes();
        let mut cursor = tree_sitter::QueryCursor::new();

        // Collect (heading_start_line, heading_text) pairs.
        let mut headings: Vec<(usize, String)> = Vec::new();
        let mut stream = cursor.matches(&query, tree.root_node(), source_bytes);
        while let Some(m) = stream.next() {
            let mut heading_text = String::new();
            let mut start_line = 0usize;

            for cap in m.captures {
                let name = match capture_names.get(cap.index as usize).map(|s| s.as_str()) {
                    Some(n) => n,
                    None => continue,
                };
                match name {
                    "heading" => {
                        start_line = cap.node.start_position().row;
                    }
                    "heading_text" => {
                        heading_text = cap
                            .node
                            .utf8_text(source_bytes)
                            .unwrap_or("")
                            .trim()
                            .to_string();
                    }
                    _ => {}
                }
            }

            if !heading_text.is_empty() {
                headings.push((start_line, heading_text));
            }
        }

        if headings.is_empty() {
            return Ok(fallback());
        }

        let lines: Vec<&str> = source.lines().collect();
        let mut chunks = Vec::new();

        for (i, (start_line, heading_text)) in headings.iter().enumerate() {
            let end_line = if i + 1 < headings.len() {
                headings[i + 1].0.saturating_sub(1)
            } else {
                lines.len().saturating_sub(1)
            };

            let section_lines: Vec<&str> = lines
                .get(*start_line..=end_line)
                .unwrap_or_default()
                .to_vec();
            let content = section_lines.join("\n");

            if content.trim().is_empty() {
                continue;
            }

            let content_hash = sha256_bytes(content.as_bytes());
            chunks.push(CodeChunk {
                id: Uuid::new_v4(),
                file_path: file_path.to_string(),
                repo_root: repo_root.to_string(),
                language: Language::Markdown,
                chunk_type: ChunkType::Documentation,
                name: heading_text.clone(),
                parent_context: file_name(file_path).to_string(),
                content,
                start_line: start_line + 1,
                end_line: end_line + 1,
                content_hash,
                indexed_at: Utc::now(),
            });
        }

        Ok(chunks)
    }
}

// --- Helpers ----------------------------------------------------------------

/// Build a single-chunk fallback (YAML, heading-less Markdown).
fn build_single_chunk(
    source: &str,
    language: Language,
    chunk_type: ChunkType,
    name: &str,
    parent_context: String,
    file_path: &str,
    repo_root: &str,
) -> CodeChunk {
    let line_count = source.lines().count();
    let content_hash = sha256_bytes(source.as_bytes());
    CodeChunk {
        id: Uuid::new_v4(),
        file_path: file_path.to_string(),
        repo_root: repo_root.to_string(),
        language,
        chunk_type,
        name: name.to_string(),
        parent_context,
        content: source.to_string(),
        start_line: 1,
        end_line: line_count.max(1),
        content_hash,
        indexed_at: Utc::now(),
    }
}

/// Extract the display name for a semantic node by scanning the query captures
/// in the node's byte range for a `*_name` capture, then falling back to
/// walking immediate children for identifier nodes.
fn extract_node_name(
    node: tree_sitter::Node,
    source: &[u8],
    query: &tree_sitter::Query,
    capture_names: &[String],
    start_byte: usize,
    end_byte: usize,
) -> String {
    let mut cursor = tree_sitter::QueryCursor::new();
    cursor.set_byte_range(start_byte..end_byte);

    let mut stream = cursor.matches(query, node, source);
    while let Some(m) = stream.next() {
        for cap in m.captures {
            let cap_name = match capture_names.get(cap.index as usize) {
                Some(n) => n.as_str(),
                None => continue,
            };
            if cap_name.ends_with("_name") || cap_name == "heading_text" {
                let text = cap.node.utf8_text(source).unwrap_or("").trim().to_string();
                if !text.is_empty() {
                    return text;
                }
            }
        }
    }

    // Fallback: walk immediate children for any identifier-like node.
    let mut child_cursor = node.walk();
    for child in node.children(&mut child_cursor) {
        let kind = child.kind();
        if matches!(
            kind,
            "identifier"
                | "type_identifier"
                | "field_identifier"
                | "property_identifier"
        ) {
            let text = child.utf8_text(source).unwrap_or("").trim().to_string();
            if !text.is_empty() {
                return text;
            }
        }
    }

    node.kind().to_string()
}

/// Compute SHA-256 of the given bytes, returning raw digest bytes.
pub fn sha256_bytes(data: &[u8]) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finalize().to_vec()
}

/// Extract the filename (without directory) from a path string.
pub fn file_name(path: &str) -> &str {
    std::path::Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(path)
}

/// Build the embedding text for a chunk following the sprint 003 embedding strategy.
///
/// Format:
/// ```text
/// {language} {chunk_type}: {name}
/// Parent: {parent_context}
/// {content (capped at MAX_EMBED_CHARS)}
/// ```
///
/// Returns `(text, truncated)` where `truncated` is true if content was cut.
pub fn build_embed_text(chunk: &CodeChunk) -> (String, bool) {
    let header = if chunk.parent_context.is_empty() {
        format!("{} {}: {}\n", chunk.language, chunk.chunk_type, chunk.name)
    } else {
        format!(
            "{} {}: {}\nParent: {}\n",
            chunk.language, chunk.chunk_type, chunk.name, chunk.parent_context
        )
    };

    let truncated = chunk.content.len() > MAX_EMBED_CHARS;
    let content_slice = if truncated {
        // Truncate at a character boundary within the first MAX_EMBED_CHARS bytes.
        let end = chunk.content.char_indices()
            .take_while(|(i, _)| *i < MAX_EMBED_CHARS)
            .last()
            .map(|(i, c)| i + c.len_utf8())
            .unwrap_or(MAX_EMBED_CHARS);
        &chunk.content[..end]
    } else {
        &chunk.content
    };

    (format!("{header}{content_slice}"), truncated)
}
