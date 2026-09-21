/**
 * 应用外壳：状态条（上）+ 侧边栏（左）+ 页面区（右）+ toast 容器。
 *
 * 这里是**唯一**调用 `useEventBridge()` 的地方（订阅全部后端事件 → 失效 query），
 * 页面不要各自订阅。
 */

import { Box } from "@mui/material";

import { useEventBridge } from "@/shared/api";

import { AppRouter } from "./AppRouter";
import { Sidebar } from "./Sidebar";
import { StatusBar } from "./StatusBar";
import { Toasts } from "./Toasts";

export function AppShell() {
  useEventBridge();

  return (
    <Box sx={{ display: "flex", flexDirection: "column", height: "100vh", overflow: "hidden" }}>
      <StatusBar />
      <Box sx={{ display: "flex", flex: 1, minHeight: 0 }}>
        <Sidebar />
        <Box
          component="main"
          sx={{ flex: 1, minWidth: 0, minHeight: 0, overflow: "auto", bgcolor: "background.default" }}
        >
          <AppRouter />
        </Box>
      </Box>
      <Toasts />
    </Box>
  );
}
