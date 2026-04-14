/**
 * Auto-Updater — checks a release host for newer extension versions.
 *
 * Supports two providers behind a common `ReleaseProvider` interface:
 *   - Gitea  (default) — uses `Authorization: token <value>` auth.
 *   - GitHub — uses `Authorization: Bearer <value>` + GitHub API headers.
 *
 * On activation (max once per hour):
 *   1. GET {releasesLatestUrl} with auth header (if token present)
 *   2. Compare tag_name version vs installed version
 *   3. If newer: download .vsix, install, prompt reload
 *
 * Security note: the Authorization header is ONLY sent on the first hop.
 * Redirects (GitHub assets 302 to pre-signed S3 URLs) are followed with no
 * Authorization header attached, so the token cannot leak to third-party
 * hosts and AWS will not reject the request for an unrecognised header.
 */

import * as fs from "fs";
import * as os from "os";
import * as path from "path";
import * as http from "http";
import * as https from "https";
import * as url from "url";
import { execFile } from "child_process";
import * as vscode from "vscode";
import type { OutputChannelManager } from "./outputChannel";

const CHECK_INTERVAL_MS = 3600 * 1000;
const API_TIMEOUT_MS = 5000;
const DOWNLOAD_TIMEOUT_MS = 30000;

type TokenProvider = () => Thenable<string | undefined>;
type HeaderMap = Record<string, string>;

interface ReleaseProvider {
  readonly name: "gitea" | "github";
  /** URL for `/releases/latest`. */
  readonly releasesLatestUrl: string;
  /** Auth + accept headers for API calls. Empty when no token configured. */
  apiHeaders(): Promise<HeaderMap>;
  /** Auth headers to attach to the FIRST-hop asset download (not redirects). */
  assetHeaders(): Promise<HeaderMap>;
}

class GiteaProvider implements ReleaseProvider {
  readonly name = "gitea" as const;
  readonly releasesLatestUrl: string;
  constructor(baseUrl: string, repo: string, private tokenProvider: TokenProvider) {
    const trimmed = baseUrl.replace(/\/+$/, "");
    this.releasesLatestUrl = `${trimmed}/api/v1/repos/${repo}/releases/latest`;
  }
  async apiHeaders(): Promise<HeaderMap> {
    const token = await this.tokenProvider();
    return token ? { Authorization: `token ${token}` } : {};
  }
  async assetHeaders(): Promise<HeaderMap> {
    // Gitea serves assets directly — same auth scheme as the API.
    return this.apiHeaders();
  }
}

class GithubProvider implements ReleaseProvider {
  readonly name = "github" as const;
  readonly releasesLatestUrl: string;
  constructor(repo: string, private tokenProvider: TokenProvider) {
    this.releasesLatestUrl = `https://api.github.com/repos/${repo}/releases/latest`;
  }
  async apiHeaders(): Promise<HeaderMap> {
    const token = await this.tokenProvider();
    const base: HeaderMap = {
      Accept: "application/vnd.github+json",
      "X-GitHub-Api-Version": "2022-11-28",
      "User-Agent": "yggdrasil-local-extension",
    };
    return token ? { ...base, Authorization: `Bearer ${token}` } : base;
  }
  async assetHeaders(): Promise<HeaderMap> {
    // First hop to api.github.com is authorised; the 302 to S3 is followed
    // anonymously by downloadFile (see the `followingRedirect` guard).
    return this.apiHeaders();
  }
}

export class AutoUpdater implements vscode.Disposable {
  private provider: ReleaseProvider;

  constructor(
    private context: vscode.ExtensionContext,
    private outputChannel: OutputChannelManager
  ) {
    const config = vscode.workspace.getConfiguration("yggdrasil");
    const providerName = config.get<string>("autoUpdate.provider", "gitea");

    if (providerName === "github") {
      const repo = config.get<string>("githubRepo", "");
      this.provider = new GithubProvider(repo, () =>
        context.secrets.get("yggdrasil.githubToken")
      );
    } else {
      const baseUrl = config.get<string>("giteaUrl", "http://localhost:3000");
      const repo = config.get<string>("giteaRepo", "you/Yggdrasil");
      this.provider = new GiteaProvider(baseUrl, repo, () =>
        context.secrets.get("yggdrasil.giteaToken")
      );
    }
  }

  async checkAndUpdate(): Promise<void> {
    const config = vscode.workspace.getConfiguration("yggdrasil");
    if (!config.get<boolean>("autoUpdate.enabled", true)) {
      return;
    }

    const lastCheck = this.context.globalState.get<number>("autoUpdate.lastCheck", 0);
    if (Date.now() - lastCheck < CHECK_INTERVAL_MS) {
      return;
    }
    await this.context.globalState.update("autoUpdate.lastCheck", Date.now());

    try {
      const currentVersion = this.context.extension.packageJSON.version || "0.0.0";

      const headers = await this.provider.apiHeaders();
      const release = await this.fetchJson(this.provider.releasesLatestUrl, headers);

      if (!release) {
        this.outputChannel.append(
          `Auto-update (${this.provider.name}): no release returned — check URL, network, or auth token in Settings → Secrets`
        );
        return;
      }

      const tagName = release.tag_name as string | undefined;
      if (!tagName) {
        this.outputChannel.append(`Auto-update (${this.provider.name}): release has no tag_name`);
        return;
      }

      const remoteVersion = tagName.replace(/^v/, "");
      if (!this.isNewer(remoteVersion, currentVersion)) {
        return;
      }

      this.outputChannel.append(
        `Auto-update (${this.provider.name}): ${currentVersion} → ${remoteVersion}`
      );

      const assets = (release.assets as Array<Record<string, unknown>>) || [];
      const vsixAsset = assets.find(
        (a) => typeof a.name === "string" && (a.name as string).endsWith(".vsix")
      );

      if (!vsixAsset || !vsixAsset.browser_download_url) {
        this.outputChannel.append(
          `Auto-update (${this.provider.name}): release has no .vsix asset, skipping`
        );
        return;
      }

      const tmpDir = path.join(os.tmpdir(), "ygg-update");
      if (!fs.existsSync(tmpDir)) {
        fs.mkdirSync(tmpDir, { recursive: true });
      }
      const assetName = vsixAsset.name as string;
      const downloadUrl = vsixAsset.browser_download_url as string;
      const tmpFile = path.join(tmpDir, assetName);

      const assetHeaders = await this.provider.assetHeaders();
      await this.downloadFile(downloadUrl, tmpFile, assetHeaders);
      this.outputChannel.append(
        `Auto-update (${this.provider.name}): downloaded ${assetName}`
      );

      await this.installVsix(tmpFile);

      try {
        fs.unlinkSync(tmpFile);
      } catch {
        // ignore
      }

      const action = await vscode.window.showInformationMessage(
        `Yggdrasil updated to v${remoteVersion}. Reload to activate.`,
        "Reload"
      );
      if (action === "Reload") {
        vscode.commands.executeCommand("workbench.action.reloadWindow");
      }
    } catch (err) {
      this.outputChannel.append(
        `Auto-update check failed: ${err instanceof Error ? err.message : String(err)}`
      );
    }
  }

  private isNewer(remote: string, current: string): boolean {
    const strip = (v: string) => v.split("-")[0].split("+")[0];
    const r = strip(remote).split(".").map((s) => parseInt(s, 10) || 0);
    const c = strip(current).split(".").map((s) => parseInt(s, 10) || 0);
    for (let i = 0; i < 3; i++) {
      const rv = r[i] || 0;
      const cv = c[i] || 0;
      if (rv > cv) return true;
      if (rv < cv) return false;
    }
    const rp = remote.includes("-");
    const cp = current.includes("-");
    if (!rp && cp) return true;
    return false;
  }

  /**
   * HTTP GET JSON with timeout. Surfaces auth failures to the output channel
   * with an actionable hint (configure a token in Settings → Secrets).
   */
  private fetchJson(
    requestUrl: string,
    headers: HeaderMap
  ): Promise<Record<string, unknown> | null> {
    return new Promise((resolve) => {
      const parsed = url.parse(requestUrl);
      const client = parsed.protocol === "https:" ? https : http;
      const options: http.RequestOptions = {
        ...parsed,
        headers: Object.keys(headers).length > 0 ? headers : undefined,
      };

      const timeout = setTimeout(() => resolve(null), API_TIMEOUT_MS);

      try {
        const req = client.get(options, (res) => {
          if (res.statusCode === 401 || res.statusCode === 403) {
            clearTimeout(timeout);
            res.resume();
            this.outputChannel.append(
              `Auto-update (${this.provider.name}): HTTP ${res.statusCode} — set a ${
                this.provider.name === "github" ? "GitHub" : "Gitea"
              } token in Yggdrasil → Settings → Secrets`
            );
            resolve(null);
            return;
          }
          let data = "";
          res.on("data", (chunk: Buffer) => {
            data += chunk.toString();
          });
          res.on("end", () => {
            clearTimeout(timeout);
            try {
              resolve(JSON.parse(data));
            } catch {
              resolve(null);
            }
          });
        });
        req.on("error", () => {
          clearTimeout(timeout);
          resolve(null);
        });
      } catch {
        clearTimeout(timeout);
        resolve(null);
      }
    });
  }

  /**
   * Download a file from URL to disk.
   *
   * Auth header is attached only on the FIRST hop. Redirects are followed
   * with no Authorization header so the token cannot leak to a 302-target
   * host (GitHub's asset download flow 302s to AWS S3 pre-signed URLs).
   */
  private downloadFile(
    requestUrl: string,
    dest: string,
    headers: HeaderMap,
    followingRedirect = false
  ): Promise<void> {
    return new Promise((resolve, reject) => {
      const parsed = url.parse(requestUrl);
      const client = parsed.protocol === "https:" ? https : http;
      const effectiveHeaders = followingRedirect ? {} : headers;
      const options: http.RequestOptions = {
        ...parsed,
        headers: Object.keys(effectiveHeaders).length > 0 ? effectiveHeaders : undefined,
      };
      const timeout = setTimeout(
        () => reject(new Error("download timeout")),
        DOWNLOAD_TIMEOUT_MS
      );

      try {
        const req = client.get(options, (res) => {
          if (
            res.statusCode &&
            res.statusCode >= 300 &&
            res.statusCode < 400 &&
            res.headers.location
          ) {
            clearTimeout(timeout);
            this.downloadFile(res.headers.location, dest, headers, true)
              .then(resolve)
              .catch(reject);
            res.resume();
            return;
          }

          if (res.statusCode === 401 || res.statusCode === 403) {
            clearTimeout(timeout);
            res.resume();
            reject(
              new Error(
                `HTTP ${res.statusCode} downloading asset — token may be missing or lacks scope`
              )
            );
            return;
          }

          const file = fs.createWriteStream(dest);
          res.pipe(file);
          file.on("finish", () => {
            clearTimeout(timeout);
            file.close();
            resolve();
          });
        });
        req.on("error", (err) => {
          clearTimeout(timeout);
          reject(err);
        });
      } catch (err) {
        clearTimeout(timeout);
        reject(err);
      }
    });
  }

  private installVsix(vsixPath: string): Promise<void> {
    return new Promise((resolve, reject) => {
      const codePath = this.findCodeCli();

      execFile(
        codePath,
        ["--install-extension", vsixPath, "--force"],
        { timeout: 30000 },
        (err) => {
          if (err) {
            this.outputChannel.append(
              `Auto-update: install failed via CLI, trying VS Code API`
            );
            const uri = vscode.Uri.file(vsixPath);
            vscode.commands
              .executeCommand("workbench.extensions.installExtension", uri)
              .then(
                () => resolve(),
                (e) => reject(e)
              );
          } else {
            resolve();
          }
        }
      );
    });
  }

  private findCodeCli(): string {
    return "code";
  }

  dispose(): void {
    // Nothing to clean up
  }
}
