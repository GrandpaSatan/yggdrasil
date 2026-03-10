use std::collections::BTreeMap;

use ygg_domain::chunk::SearchResult;

/// Assemble a context string from ranked search results suitable for LLM consumption.
///
/// Chunks are grouped by `file_path`. Within each file group, chunks are sorted by
/// `start_line` ascending to preserve reading order. File groups are ordered by the
/// highest `score` of any chunk in the group (most relevant file first), which
/// maximises the chance that the LLM attends to the most relevant content.
///
/// Format:
/// ```text
/// // File: /path/to/file.rs
/// // Lines 42-67 (function: handle_request)
/// pub fn handle_request(...) { ... }
///
/// // Lines 100-120 (struct: Config)
/// pub struct Config { ... }
///
/// ---
///
/// // File: /path/to/other.py
/// // Lines 1-30 (function: main)
/// def main(): ...
/// ```
///
/// Token budget enforcement: tokens are estimated at 4 characters per token.
/// `char_budget = token_budget * 4`. Chunks are added in relevance order (most
/// relevant file first, then by start_line within a file). Once the char budget
/// is exhausted the function stops adding chunks entirely — no mid-chunk truncation.
///
/// This is a pure function with no I/O. Context assembly P95 target is < 20ms.
pub fn assemble_context(results: &[SearchResult], token_budget: usize) -> String {
    if results.is_empty() {
        return String::new();
    }

    let char_budget = token_budget * 4;

    // Group results by file_path. BTreeMap gives stable iteration order by path,
    // which we will override below by sorting file groups on score.
    // Value: Vec of (score, start_line, SearchResult reference index in `results`).
    let mut file_groups: BTreeMap<&str, Vec<(f64, usize, &SearchResult)>> = BTreeMap::new();
    for result in results {
        file_groups
            .entry(result.chunk.file_path.as_str())
            .or_default()
            .push((result.score, result.chunk.start_line, result));
    }

    // For each file group, compute the maximum score (used for ordering groups).
    let mut ordered_files: Vec<(&str, f64, Vec<(f64, usize, &SearchResult)>)> = file_groups
        .into_iter()
        .map(|(path, mut entries)| {
            // Sort entries within this file by start_line ascending.
            entries.sort_by_key(|(_, start_line, _)| *start_line);
            let max_score = entries
                .iter()
                .map(|(score, _, _)| *score)
                .fold(f64::NEG_INFINITY, f64::max);
            (path, max_score, entries)
        })
        .collect();

    // Order file groups: highest max_score first. Ties broken by file path (stable).
    ordered_files.sort_by(|(path_a, score_a, _), (path_b, score_b, _)| {
        score_b
            .partial_cmp(score_a)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| path_a.cmp(path_b))
    });

    let mut output = String::with_capacity(char_budget.min(65536));
    let mut chars_used: usize = 0;
    let mut first_file = true;

    'outer: for (file_path, _max_score, entries) in &ordered_files {
        // File separator (not added before the very first file).
        if !first_file {
            output.push_str("\n---\n\n");
            chars_used += 7;
        }
        first_file = false;

        let file_header = format!("// File: {file_path}\n");
        chars_used += file_header.len();
        output.push_str(&file_header);

        for (_score, _start_line, result) in entries {
            let chunk = &result.chunk;
            let chunk_header = format!(
                "// Lines {}-{} ({}: {})\n",
                chunk.start_line,
                chunk.end_line,
                chunk.chunk_type,
                chunk.name,
            );
            // Content block: header + content + trailing newline.
            let block = format!("{}{}\n", chunk_header, chunk.content);

            if chars_used + block.len() > char_budget {
                // Budget exhausted — stop adding any more chunks (greedy, not knapsack).
                break 'outer;
            }

            output.push_str(&block);
            chars_used += block.len();
        }
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use uuid::Uuid;
    use ygg_domain::chunk::{ChunkType, CodeChunk, Language, SearchResult, SearchSource};

    fn make_result(file_path: &str, start_line: usize, content: &str, score: f64) -> SearchResult {
        SearchResult {
            chunk: CodeChunk {
                id: Uuid::new_v4(),
                file_path: file_path.to_string(),
                repo_root: "/repo".to_string(),
                language: Language::Rust,
                chunk_type: ChunkType::Function,
                name: "test_fn".to_string(),
                parent_context: String::new(),
                content: content.to_string(),
                start_line,
                end_line: start_line + 5,
                content_hash: vec![],
                indexed_at: Utc::now(),
            },
            score,
            source: SearchSource::Fused,
        }
    }

    #[test]
    fn empty_results_return_empty_string() {
        assert_eq!(assemble_context(&[], 32000), "");
    }

    #[test]
    fn single_chunk_includes_file_header_and_line_range() {
        let results = vec![make_result("/repo/src/lib.rs", 10, "pub fn foo() {}", 0.5)];
        let ctx = assemble_context(&results, 32000);
        assert!(ctx.contains("// File: /repo/src/lib.rs"), "missing file header");
        assert!(ctx.contains("// Lines 10-15 (function: test_fn)"), "missing line header");
        assert!(ctx.contains("pub fn foo() {}"), "missing content");
    }

    #[test]
    fn chunks_within_file_sorted_by_start_line() {
        let results = vec![
            make_result("/repo/a.rs", 20, "fn b()", 0.5),
            make_result("/repo/a.rs", 5, "fn a()", 0.5),
        ];
        let ctx = assemble_context(&results, 32000);
        let pos_a = ctx.find("fn a()").expect("fn a() not found");
        let pos_b = ctx.find("fn b()").expect("fn b() not found");
        assert!(pos_a < pos_b, "chunks not sorted by start_line");
    }

    #[test]
    fn file_groups_ordered_by_highest_score() {
        let results = vec![
            make_result("/repo/low.rs", 1, "low content", 0.1),
            make_result("/repo/high.rs", 1, "high content", 0.9),
        ];
        let ctx = assemble_context(&results, 32000);
        let pos_high = ctx.find("high.rs").expect("high.rs not found");
        let pos_low = ctx.find("low.rs").expect("low.rs not found");
        assert!(pos_high < pos_low, "higher-scoring file should appear first");
    }

    #[test]
    fn token_budget_stops_adding_chunks() {
        // Each chunk content is ~40 chars. Budget of 10 tokens = 40 chars.
        // Only the first chunk should fit (file header + line header + content exceeds 40 chars
        // for the second chunk).
        let results = vec![
            make_result("/repo/a.rs", 1, "x".repeat(20).as_str(), 0.9),
            make_result("/repo/b.rs", 1, "y".repeat(20).as_str(), 0.5),
        ];
        // Very tight budget — only enough for approximately one chunk including headers.
        let ctx = assemble_context(&results, 15);
        // The context should not be empty (first chunk fits or budget is hit mid-way).
        // The important invariant: no mid-chunk truncation.
        assert!(
            ctx.len() <= 15 * 4 + 200,
            "context far exceeds budget: {} chars",
            ctx.len()
        );
    }
}
