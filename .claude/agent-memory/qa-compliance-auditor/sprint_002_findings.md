# Sprint 002 Detailed Findings

## FAIL: LSH Backfill Timing
- Sprint doc decision log: "Start HTTP server before LSH backfill completes"
- Sprint doc risks: "Backfill runs after the HTTP server is bound"
- Actual: `AppState::new()` runs backfill synchronously before server binds
- Comment in `state.rs:75-76` incorrectly claims server binds first
- Impact: Startup blocked by backfill; not an issue for MVP with empty DB but violates stated design

## Advisory: SHA-256 Separator
- Sprint doc: `format!("{}{}", body.cause, body.effect)` (no separator)
- Actual: `cause + "\n" + effect` (newline separator)
- Assessment: Improvement -- prevents ambiguous collisions. Not a FAIL.

## Advisory: `is_empty()` on LshIndex
- Not specified in sprint doc's interface definition
- Required by clippy when `len()` is implemented
- Assessment: Standard Rust boilerplate, not unauthorized addition.

## PASS items of note
- No unwrap() on user input paths
- Proper error logging with tracing (error for Store/Embed/Config, warn for Validation)
- CORS layer is permissive as specified
- Graceful shutdown with SIGTERM/SIGINT handling
- Fire-and-forget LSH persistence (not on critical path)
- Access count bumping via spawned task
