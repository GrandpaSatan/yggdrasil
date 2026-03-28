/**
 * screenshot tool — Headless Chromium page capture via Puppeteer.
 *
 * Port of ygg-mcp/src/tools.rs screenshot function to TypeScript.
 * Uses puppeteer-core (no bundled browser — connects to system Chrome/Chromium).
 */

import * as fs from "fs";
import * as path from "path";
import puppeteer, { type Browser } from "puppeteer-core";

const SCREENSHOTS_DIR = "/tmp/ygg-screenshots";

interface ScreenshotParams {
  url: string;
  selector?: string;
  full_page?: boolean;
  viewport_width?: number;
  viewport_height?: number;
}

// Lazy browser singleton — launched once, reused across calls.
let browserInstance: Browser | null = null;

/** Find a Chrome/Chromium executable on the system. */
function findChrome(): string {
  const candidates = [
    "/usr/bin/chromium-browser",
    "/usr/bin/chromium",
    "/usr/bin/google-chrome-stable",
    "/usr/bin/google-chrome",
    "/snap/bin/chromium",
  ];
  for (const c of candidates) {
    if (fs.existsSync(c)) return c;
  }
  throw new Error(
    "Chrome/Chromium not found. Install with: sudo apt install chromium-browser"
  );
}

/** Get or launch the browser singleton. */
async function getBrowser(): Promise<Browser> {
  if (browserInstance?.connected) return browserInstance;

  const execPath = findChrome();
  browserInstance = await puppeteer.launch({
    executablePath: execPath,
    headless: true,
    args: [
      "--no-sandbox",
      "--disable-gpu",
      "--disable-dev-shm-usage",
      "--disable-setuid-sandbox",
    ],
  });

  return browserInstance;
}

/** Generate a filename from URL + timestamp. */
function slugFilename(url: string): string {
  const slug = url
    .replace(/^https?:\/\//, "")
    .replace(/[^a-zA-Z0-9]+/g, "-")
    .replace(/-+/g, "-")
    .replace(/^-|-$/g, "")
    .slice(0, 60);
  const ts = new Date().toISOString().replace(/[:.]/g, "-").slice(0, 19);
  return `${slug}-${ts}.png`;
}

export async function handleScreenshot(
  params: ScreenshotParams
): Promise<string> {
  const browser = await getBrowser();

  const width = params.viewport_width ?? 1280;
  const height = params.viewport_height ?? 720;
  const fullPage = params.full_page ?? false;

  // Create output directory
  fs.mkdirSync(SCREENSHOTS_DIR, { recursive: true });

  const page = await browser.newPage();
  try {
    await page.setViewport({ width, height });

    // Navigate with 30s timeout
    await page.goto(params.url, {
      waitUntil: "networkidle2",
      timeout: 30_000,
    });

    // Wait for optional selector
    if (params.selector) {
      try {
        await page.waitForSelector(params.selector, { timeout: 10_000 });
      } catch {
        // Selector not found — capture anyway, note in output
      }
    }

    // Capture screenshot
    const filename = slugFilename(params.url);
    const outputPath = path.join(SCREENSHOTS_DIR, filename);

    await page.screenshot({
      path: outputPath,
      fullPage,
    });

    return `Screenshot saved to ${outputPath}\n\nUse the Read tool to view the image.`;
  } finally {
    await page.close();
  }
}
