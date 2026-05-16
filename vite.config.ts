import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { resolve } from "node:path";

// Tauri expects a fixed port, fail if that port is not available
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
  },
  envPrefix: ["VITE_", "TAURI_"],
  build: {
    target: "esnext",
    minify: !process.env.TAURI_DEBUG ? "esbuild" : false,
    sourcemap: !!process.env.TAURI_DEBUG,
    // Multi-page setup so terminal.html and chat.html bundle as real
    // entry points (not static public/ assets) and can import the
    // shared terminal-runtime TypeScript module. See ADR-0006 and
    // docs/design/terminal-runtime.md.
    rollupOptions: {
      input: {
        main: resolve(__dirname, "index.html"),
        terminal: resolve(__dirname, "terminal.html"),
        chat: resolve(__dirname, "chat.html"),
      },
    },
  },
});
