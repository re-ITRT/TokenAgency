import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";
import path from "node:path";

// Tauri 会注入 window.__TAURI__；前端只通过 src/shared/api 访问它。
export default defineConfig({
  plugins: [react()],
  resolve: {
    alias: { "@": path.resolve(__dirname, "./src") },
  },
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    watch: { ignored: ["**/src-tauri/**", "**/target/**"] },
  },
  test: {
    // 只测**我们的**代码。`.ta-tmp/` 是调研时下载的第三方仓库（round 312 的
    // `cline` / `OpenTokenUsage` 等），它们自带一堆跑不通的测试 —— 混进来会让
    // 「前端全绿」这个信号失去意义（既可能假报错，也可能掩盖真错）。
    exclude: ["**/node_modules/**", "**/dist/**", "**/target/**", "**/.ta-tmp/**", "src-tauri/**"],
  },
  build: {
    target: "esnext",
    sourcemap: false,
    chunkSizeWarningLimit: 900,
    rollupOptions: {
      output: {
        // MUI 与图表是大头，必须分块（首屏 JS 预算 < 400KB gzip）
        manualChunks: {
          mui: ["@mui/material", "@mui/icons-material", "@emotion/react", "@emotion/styled"],
          charts: ["echarts"],
          vendor: ["react", "react-dom", "react-router-dom"],
        },
      },
    },
  },
});
