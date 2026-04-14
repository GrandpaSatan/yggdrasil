import { defineConfig } from "vitest/config";
import path from "path";

export default defineConfig({
  test: {
    environment: "node",
    globals: true,
    include: ["src/**/*.test.ts"],
    alias: {
      // Route all `import ... from "vscode"` to our stub
      vscode: path.resolve(__dirname, "src/__mocks__/vscode.ts"),
    },
  },
});
