/**
 * 入口：只负责 QueryClient + 挂载。
 *
 * **主题只有一个来源**：`App.tsx` 里的 `<ThemeProvider theme={buildTheme(themeMode)}>`
 * （跟随 `useUiStore.themeMode`）。这里不再包一层 `ThemeProvider`/`CssBaseline` ——
 * 两处建主题会让"跟随用户选择"与"固定深色"打架，也会多一份 Emotion 缓存。
 */

import React from "react";
import ReactDOM from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import App from "./App";

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      // 本地 IPC：数据变化靠事件失效，轮询只是兜底
      staleTime: 5_000,
      refetchOnWindowFocus: false,
      retry: 1,
    },
  },
});

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <QueryClientProvider client={queryClient}>
      <App />
    </QueryClientProvider>
  </React.StrictMode>,
);
