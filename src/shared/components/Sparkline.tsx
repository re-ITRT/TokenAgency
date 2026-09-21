/**
 * 迷你趋势线（数字卡下方的 ▁▂▃▅▆▇█）。
 *
 * 为什么不用 ECharts：一个 60×20 的 sparkline 用 echarts 要 ~300KB 运行时 + 一次实例化，
 * 而数字卡是**每秒级**刷新的。纯 SVG polyline 零依赖、零重排，符合 [07 §1.3]
 * 「一张主图，其余是数字」的取舍。
 */

import { Box } from "@mui/material";

export type SparklineProps = {
  /** 按时间升序的数值序列。空数组/全等值/NaN 都不会崩。 */
  data: readonly number[];
  width?: number;
  height?: number;
  /** 不传则跟随主题主色 */
  color?: string;
  /** 填充面积（默认开，视觉上更稳） */
  area?: boolean;
  label?: string;
};

export function Sparkline({
  data,
  width = 88,
  height = 22,
  color,
  area = true,
  label,
}: SparklineProps) {
  const points = data.filter((v) => Number.isFinite(v));
  const stroke = color ?? "currentColor";

  if (points.length === 0) {
    // 空状态也占位，避免卡片高度跳动
    return <Box sx={{ width, height, flex: "0 0 auto" }} aria-hidden />;
  }

  const min = Math.min(...points);
  const max = Math.max(...points);
  const span = max - min || 1; // 全等值时画中线，不除零
  const stepX = points.length > 1 ? width / (points.length - 1) : width;
  const pad = 1.5; // 留一点描边余量，避免被裁

  const coords = points.map((v, i) => {
    const x = points.length > 1 ? i * stepX : width / 2;
    const y = height - pad - ((v - min) / span) * (height - pad * 2);
    return { x, y };
  });

  const first = coords[0];
  const last = coords[coords.length - 1];
  const line = coords.map((p) => `${p.x.toFixed(2)},${p.y.toFixed(2)}`).join(" ");
  const areaPath =
    first && last
      ? `M ${first.x.toFixed(2)},${height} L ${line.replace(/ /g, " L ")} L ${last.x.toFixed(2)},${height} Z`
      : "";

  return (
    <Box
      component="svg"
      role="img"
      aria-label={label}
      viewBox={`0 0 ${width} ${height}`}
      width={width}
      height={height}
      sx={{ display: "block", flex: "0 0 auto", color: stroke, overflow: "visible" }}
    >
      {area && areaPath ? (
        <path d={areaPath} fill="currentColor" opacity={0.14} stroke="none" />
      ) : null}
      <polyline
        points={line}
        fill="none"
        stroke="currentColor"
        strokeWidth={1.5}
        strokeLinejoin="round"
        strokeLinecap="round"
      />
    </Box>
  );
}
