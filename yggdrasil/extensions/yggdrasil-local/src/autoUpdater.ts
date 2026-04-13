/**
 * Auto-Updater — checks Gitea releases for newer extension versions.
 *
 * On activation (max once per hour):
 * 1. GET /api/v1/repos/:owner/:repo/releases/latest from Gitea
 * 2. Compare tag_name version vs installed version
 * 3. If newer: download .vsix, install, prompt reload
 */

import * as fs from "fs";
import * as os from "os";
import * as path from "path";
import * as http from "http";
import * as https from "https";
import { execFile } from "child_process";
import * as vscode from "vscode";
import type { OutputChannelManager } from "./outputChannel";

const CHECK_INTERVAL_MS = 3600 * 1000; // 1 hour
const API_TIMEOUT_MS = 5000;
const DOWNLOAD_TIMEOUT_MS = 30000;

export class AutoUpdater implements vscode.Disposable {
  constructor(
    private context: vscode.ExtensionContext,
    private outputChannel: OutputChannelManager
  ) {}

  async checkAndUpdate(): Promise<void> {
    const config = vscode.workspace.getConfiguration("yggdrasil");
    if (!config.get<boolean>("autoUpdate.enabled", true)) {
      return;
    }

    // Rate limit: max once per hour
    const lastCheck = this.context.globalState.get<number>(
      "autoUpdate.lastCheck",
      0
    );
    if (Date.now() - lastCheck < CHECK_INTERVAL_MS) {
      return;
    }
    await this.context.globalState.update("autoUpdate.lastCheck", Date.now());

    try {
      const giteaUrl = config.get<string>(
        "giteaUrl",
        "http://10.0.65.11:3000"
      );
      const giteaRepo = config.get<string>("giteaRepo", "jesus/Yggdrasil");
      const currentVersion =
        this.context.extension.packageJSON.version || "0.0.0";

      // Fetch latest release
      const apiUrl = `${giteaUrl}/api/v1/repos/${giteaRepo}/releases/latest`;
      const release = await this.fetchJson(apiUrl);

      if (!release) {
        this.outputChannel.append("Auto-update: no releases found");
        return;
      }

      const tagName = release.tag_name as string | undefined;
      if (!tagName) {
        this.outputChannel.append("Auto-update: release has no tag_name");
        return;
      }

      const remoteVersion = tagName.replace(/^v/, "");
      if (!this.isNewer(remoteVersion, currentVersion)) {
        return; // Already up to date
      }

      this.outputChannel.append(
        `Auto-update: ${currentVersion} → ${remoteVersion}`
      );

      // Find .vsix asset
      const assets = (release.assets as Array<Record<string, unknown>>) || [];
      const vsixAsset = assets.find(
        (a) => typeof a.name === "string" && (a.name as string).endsWith(".vsix")
      );

      if (!vsixAsset || !vsixAsset.browser_download_url) {
        this.outputChannel.append(
          "Auto-update: release has no .vsix asset, skipping"
        );
        return;
      }

      // Download .vsix to temp file
      const tmpDir = path.join(os.tmpdir(), "ygg-update");
      if (!fs.existsSync(tmpDir)) {
        fs.mkdirSync(tmpDir, { recursive: true });
      }
      const assetName = vsixAsset.name as string;
      const downloadUrl = vsixAsset.browser_download_url as string;
      const tmpFile = path.join(tmpDir, assetName);

      await this.downloadFile(downloadUrl, tmpFile);
      this.outputChannel.append(`Auto-update: downloaded ${assetName}`);

      // Install via code CLI
      await this.installVsix(tmpFile);

      // Cleanup
      try {
        fs.unlinkSync(tmpFile);
      } catch {
        // ignore
      }

      // Notify user
      const action = await vscode.window.showInformationMessage(
        `Yggdrasil updated to v${remoteVersion}. Reload to activate.`,
        "Reload"
      );
      if (action === "Reload") {
        vscode.commands.executeCommand("workbench.action.reloadWindow");
      }
    } catch (err) {
      // Silent failure — log but don't crash
      this.outputChannel.append(
        `Auto-update check failed: ${err instanceof Error ? err.message : String(err)}`
      );
    }
  }

  /** Simple semver comparison: returns true if remote > current. */
  private isNewer(remote: string, current: string): boolean {
    const r = remote.split(".").map(Number);
    const c = current.split(".").map(Number);
    for (let i = 0; i < 3; i++) {
      const rv = r[i] || 0;
      const cv = c[i] || 0;
      if (rv > cv) return true;
      if (rv < cv) return false;
    }
    return false;
  }

  /** HTTP GET JSON with timeout. */
  private fetchJson(url: string): Promise<Record<string, unknown> | null> {
    return new Promise((resolve) => {
      const timeout = setTimeout(() => resolve(null), API_TIMEOUT_MS);
      const client = url.startsWith("https") ? https : http;

      try {
        const req = client.get(url, (res) => {
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

  /** Download a file from URL to disk. */
  private downloadFile(url: string, dest: string): Promise<void> {
    return new Promise((resolve, reject) => {
      const timeout = setTimeout(
        () => reject(new Error("download timeout")),
        DOWNLOAD_TIMEOUT_MS
      );
      const client = url.startsWith("https") ? https : http;

      try {
        const req = client.get(url, (res) => {
          // Follow redirects (Gitea uses 302 for asset downloads)
          if (
            res.statusCode &&
            res.statusCode >= 300 &&
            res.statusCode < 400 &&
            res.headers.location
          ) {
            clearTimeout(timeout);
            this.downloadFile(res.headers.location, dest)
              .then(resolve)
              .catch(reject);
            res.resume();
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

  /** Install a .vsix file via the code CLI. */
  private installVsix(vsixPath: string): Promise<void> {
    return new Promise((resolve, reject) => {
      // Try to find the code CLI
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
            // Fallback: try VS Code command
            const uri = vscode.Uri.file(vsixPath);
            vscode.commands
              .executeCommand(
                "workbench.extensions.installExtension",
                uri
              )
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

  /** Find the 'code' CLI binary. */
  private findCodeCli(): string {
    // VS Code sets VSCODE_IPC_HOOK which tells us it's running
    // The 'code' binary is usually in PATH on Linux
    return "code";
  }

  dispose(): void {
    // Nothing to clean up
  }
}
