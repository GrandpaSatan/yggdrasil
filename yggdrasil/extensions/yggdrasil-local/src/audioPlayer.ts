/**
 * Audio cue player for memory operations.
 *
 * Plays a short sound via aplay (Linux) when configured events occur.
 * Disabled by default — enable via yggdrasil.notifications.sound.
 */

import * as vscode from "vscode";
import { execFile } from "child_process";
import type { YggEvent } from "./eventWatcher";

export class AudioPlayer {
  onEvent(event: YggEvent): void {
    const config = vscode.workspace.getConfiguration("yggdrasil");
    if (!config.get<boolean>("notifications.sound", false)) return;

    // Only play sound for store events
    if (event.event === "ingest" && event.data.stored) {
      this.playSystemSound();
    } else if (event.event === "error") {
      this.playSystemSound();
    }
  }

  private playSystemSound(): void {
    // Use paplay (PulseAudio) with a system sound, or aplay as fallback.
    // GNOME provides standard sounds at /usr/share/sounds/
    const soundFile = "/usr/share/sounds/freedesktop/stereo/message-new-instant.oga";
    execFile("paplay", [soundFile], (err) => {
      if (err) {
        // Fallback: try the bell character via terminal
        process.stdout.write("\x07");
      }
    });
  }
}
