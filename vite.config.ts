import react from "@vitejs/plugin-react";
import { defineConfig } from "vitest/config";
// @ts-expect-error no @types/node in this project; only `env` is used here
import process from "node:process";

const host = process.env.TAURI_DEV_HOST;

// 1430, not the scaffold's 1420. Demo Studio's CLAUDE.md reserves 1420-1422 (Aide, the
// owner's own `tauri dev`, and main-tree Playwright), and `strictPort` turns an overlap into a
// hard failure rather than a quiet second server. Pigeon takes a number outside that block.
const DEV_PORT = 1430;

export default defineConfig(() => ({
  plugins: [react()],

  // Do not let Vite scroll Rust errors off the screen.
  clearScreen: false,
  server: {
    port: DEV_PORT,
    strictPort: true,
    host: host || false,
    hmr: host ? { protocol: "ws", host, port: DEV_PORT + 1 } : undefined,
    watch: {
      ignored: ["**/src-tauri/**"],
    },
  },
  test: {
    environment: "jsdom",
    globals: true,
    setupFiles: ["./src/test/setup.ts"],
    include: ["src/**/*.test.{ts,tsx}"],
  },
}));
