/**
 * Minimal vscode module stub for Vitest.
 *
 * Only the surface used by the modules under test is implemented.
 * Add stubs here as new tests need them.
 */

// ── Clipboard ────────────────────────────────────────────────
let _clipboardText = "";

export const env = {
  clipboard: {
    writeText: async (text: string): Promise<void> => {
      _clipboardText = text;
    },
    readText: async (): Promise<string> => {
      return _clipboardText;
    },
  },
};

// ── Configuration ────────────────────────────────────────────
const _configStore: Record<string, unknown> = {
  "odinUrl": "http://localhost:8080",
  "mimirUrl": "http://localhost:9090",
  "huginUrl": "http://localhost:11434",
  "giteaUrl": "http://localhost:3000",
  "hooks.managed": true,
  "hooks.writeMode": "merge",
};

export const workspace = {
  getConfiguration: (_section?: string) => ({
    get: <T>(key: string, defaultValue?: T): T => {
      const full = _section ? `${_section}.${key}` : key;
      const raw = _configStore[key] ?? _configStore[full];
      return (raw !== undefined ? raw : defaultValue) as T;
    },
    update: async (key: string, value: unknown): Promise<void> => {
      _configStore[key] = value;
    },
  }),
  workspaceFolders: undefined as undefined | Array<{ uri: { fsPath: string } }>,
};

// ── Window ───────────────────────────────────────────────────
export const window = {
  showInformationMessage: async (msg: string, ..._rest: string[]): Promise<string | undefined> => {
    return undefined;
  },
  showWarningMessage: async (msg: string, ..._rest: string[]): Promise<string | undefined> => {
    return undefined;
  },
  showErrorMessage: async (msg: string, ..._rest: string[]): Promise<string | undefined> => {
    return undefined;
  },
  createOutputChannel: (_name: string) => ({
    appendLine: (_msg: string) => {},
    append: (_msg: string) => {},
    show: () => {},
    dispose: () => {},
  }),
};

// ── ConfigurationTarget ──────────────────────────────────────
export const ConfigurationTarget = {
  Global: 1,
  Workspace: 2,
  WorkspaceFolder: 3,
} as const;

// ── Uri ──────────────────────────────────────────────────────
export const Uri = {
  file: (p: string) => ({ fsPath: p, toString: () => `file://${p}` }),
  joinPath: (base: { fsPath: string }, ...parts: string[]) => {
    const joined = [base.fsPath, ...parts].join("/");
    return { fsPath: joined, toString: () => `file://${joined}` };
  },
};

// ── ExtensionContext stub ─────────────────────────────────────
const _globalStateStore = new Map<string, unknown>();
const _secretsStore = new Map<string, string>();

export function makeExtensionContext(extensionPath = "/tmp/test-extension") {
  return {
    extensionPath,
    extensionUri: Uri.file(extensionPath),
    globalStorageUri: Uri.file("/tmp/test-global-storage"),
    secrets: {
      get: async (key: string): Promise<string | undefined> => _secretsStore.get(key),
      store: async (key: string, value: string): Promise<void> => {
        _secretsStore.set(key, value);
      },
      delete: async (key: string): Promise<void> => {
        _secretsStore.delete(key);
      },
    },
    globalState: {
      get: <T>(key: string, defaultValue?: T): T =>
        (_globalStateStore.has(key) ? _globalStateStore.get(key) : defaultValue) as T,
      update: async (key: string, value: unknown): Promise<void> => {
        _globalStateStore.set(key, value);
      },
    },
    subscriptions: [] as { dispose(): void }[],
  };
}

// ── ViewColumn ───────────────────────────────────────────────
export const ViewColumn = { One: 1, Two: 2, Three: 3 } as const;

// ── Disposable ───────────────────────────────────────────────
export class Disposable {
  constructor(public callOnDispose: () => void) {}
  dispose(): void {
    this.callOnDispose();
  }
}
