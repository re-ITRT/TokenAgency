import { Box, Paper, Stack, Typography } from "@mui/material";

/**
 * W0 骨架页。
 * 工作包 F1 会把它替换成完整路由（10 个页面，见 docs/05-ui-tray.md §2）。
 */
export default function App() {
  return (
    <Box sx={{ p: 3 }}>
      <Paper sx={{ p: 3 }}>
        <Stack spacing={1}>
          <Typography variant="h5">TokenAgency</Typography>
          <Typography variant="body2" color="text.secondary">
            工作区骨架已就位。接下来由工作包 F1–F4 填充页面。
          </Typography>
          <Typography variant="caption" color="text.secondary">
            接口契约：docs/10-interface-spec.md · 内部契约：docs/11-internal-contracts.md
          </Typography>
        </Stack>
      </Paper>
    </Box>
  );
}
