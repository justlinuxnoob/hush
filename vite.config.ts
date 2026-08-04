import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Tauri serves the frontend from a fixed dev port and expects a static build in dist/.
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    watch: { ignored: ["**/src-tauri/**"] },
  },
  build: {
    // Match the oldest webview we support (WebKitGTK on Linux, WebView2 on Windows).
    target: "es2021",
    sourcemap: false,
    emptyOutDir: true,
  },
});
