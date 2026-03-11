# Sprint 028: Screenshot Review Tool

**Status:** Active
**Date:** 2026-03-11
**Project:** yggdrasil

---

## Problem

No way to visually capture and review web UI work within the Claude Code workflow. When building dashboards or web interfaces, feedback is purely textual — developers can't point at a specific element and say "move this" or "this is wrong." A visual review loop would dramatically speed up UI iteration.

## Solution

Add a `screenshot_tool` to `YggdrasilLocalServer` that captures web pages via headless Chromium (CDP). Combined with Claude Code's built-in multimodal image reading, this enables a visual feedback loop:

1. `screenshot_tool(url)` → captures page → saves PNG → returns file path
2. Claude reads the PNG via Read tool → provides visual feedback
3. User annotates the image (circles, arrows, strikethroughs) with any image editor
4. User provides the annotated image path → Claude reads it → interprets visual markup → generates targeted code changes

### Technical Approach

Use the `chromiumoxide` crate (async Chromium DevTools Protocol client) for:
- Headless Chromium launch and tab management
- Navigation with configurable wait conditions (selector, networkidle)
- Viewport and full-page screenshot capture
- Lazy browser init (first call launches, subsequent calls reuse)

### Why MCP Tool (not Bash script)

- Discoverable: Claude Code sees `screenshot_tool` in the tool list and knows when to use it
- Typed params: structured schema with validation
- Browser reuse: persistent Chromium instance across calls within a session
- Consistent: fits the Yggdrasil local MCP server architecture

---

## Changes

### 1. Add `chromiumoxide` dependency

**File:** `crates/ygg-mcp/Cargo.toml`

```toml
chromiumoxide = { version = "0.7", features = ["tokio-runtime"], default-features = false }
```

### 2. `ScreenshotParams` and `screenshot()` implementation

**File:** `crates/ygg-mcp/src/tools.rs`

```rust
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ScreenshotParams {
    /// URL to capture (e.g. "http://localhost:3000/dashboard")
    pub url: String,
    /// Optional CSS selector to wait for before capture (for SPAs)
    #[serde(default)]
    pub selector: Option<String>,
    /// Capture full scrollable page instead of just viewport (default: false)
    #[serde(default)]
    pub full_page: Option<bool>,
    /// Viewport width in pixels (default: 1280)
    #[serde(default)]
    pub viewport_width: Option<u32>,
    /// Viewport height in pixels (default: 720)
    #[serde(default)]
    pub viewport_height: Option<u32>,
}
```

Implementation:
- Navigate to URL
- If `selector` provided, wait for element (with 10s timeout)
- Set viewport dimensions
- Capture screenshot (viewport or full-page)
- Save to `/tmp/ygg-screenshots/{url_slug}-{timestamp}.png`
- Return the file path as tool result

### 3. Browser lifecycle in `YggdrasilLocalServer`

**File:** `crates/ygg-mcp/src/local_server.rs`

Add `browser: Arc<tokio::sync::OnceCell<Browser>>` to server state.

- Lazy init: first `screenshot_tool` call launches headless Chromium
- Reuse: subsequent calls open new tabs on the existing browser
- Error: if Chromium not found, return clear error message with install instructions

```rust
pub struct YggdrasilLocalServer {
    client: Client,
    config: McpServerConfig,
    tool_router: ToolRouter<Self>,
    session_id: String,
    browser: Arc<tokio::sync::OnceCell<chromiumoxide::Browser>>,
}
```

### 4. Register `screenshot_tool` in tool router

**File:** `crates/ygg-mcp/src/local_server.rs`

```rust
#[tool_router]
impl YggdrasilLocalServer {
    #[tool(description = "Capture a screenshot of a web page via headless Chromium. ...")]
    async fn screenshot_tool(&self, Parameters(params): Parameters<ScreenshotParams>) -> String {
        // ...
    }

    #[tool(description = "Sprint lifecycle doc agent. ...")]
    async fn sync_docs_tool(&self, ...) -> String { ... }
}
```

---

## Workflow Example

```
User: "Take a screenshot of http://localhost:3000/dashboard"
→ screenshot_tool(url: "http://localhost:3000/dashboard")
→ Saves /tmp/ygg-screenshots/dashboard-1741700000.png
→ Returns: "/tmp/ygg-screenshots/dashboard-1741700000.png"

Claude: *reads image via Read tool*
Claude: "The sidebar nav is overlapping the main content. KPI cards look good
         but the chart colors are too similar to distinguish."

User: *opens PNG in image editor, circles the overlap, draws X on charts*
User: "Here's my markup" → provides annotated image path

Claude: *reads annotated image*
Claude: "I see you've circled the sidebar overlap area and crossed out the
         chart section. Let me fix the sidebar z-index and update the
         chart color palette..."
→ Edits the relevant CSS/components
```

---

## Prerequisites

- Chromium or Google Chrome installed on the workstation (`chromium-browser` or `google-chrome`)
- The `chromiumoxide` crate auto-detects the Chrome binary path

## Files Modified

| File | Change |
|:---|:---|
| `crates/ygg-mcp/Cargo.toml` | Add `chromiumoxide` dependency |
| `crates/ygg-mcp/src/tools.rs` | `ScreenshotParams`, `screenshot()` fn |
| `crates/ygg-mcp/src/local_server.rs` | Add `screenshot_tool`, browser lifecycle (`Arc<OnceCell<Browser>>`) |

## Verification

- [ ] `screenshot_tool(url: "https://example.com")` captures and returns valid PNG path
- [ ] `screenshot_tool(url: "...", selector: "#main")` waits for element before capture
- [ ] `screenshot_tool(url: "...", full_page: true)` captures full scrollable page
- [ ] Custom viewport dimensions work
- [ ] Claude can read the returned PNG via Read tool
- [ ] Browser reuse works across multiple calls in same session
- [ ] Clear error message when Chromium not installed
- [ ] Screenshots saved with readable filenames and timestamps
