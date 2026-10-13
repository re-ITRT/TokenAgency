// @vitest-environment jsdom
/**
 * 令牌守卫：**`src/theme.ts` 里的值必须是 [15] 文档里的值**。
 *
 * 为什么值得单独守：改一个十六进制、加半个刻度（`p: 1.5`）、把字重写成 600，
 * 都不会报错、不影响功能 —— 但"不一致"正是"不高级"的定义（立意）。
 * `design.guard.test.ts` 管**组件有没有乱写**；这一份管**令牌本身有没有漂移**。
 *
 * 另外它会在**深/浅两态各渲染一遍**核心组件：颜色令牌路径写错时 MUI 会抛错/静默丢色，
 * 只看 tsc 是看不出来的。
 */

import { createTheme, ThemeProvider, type Theme } from "@mui/material/styles";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeAll, describe, expect, it } from "vitest";

import App from "@/App";
import { NumberCard, StatusDot } from "@/shared/components";
import { registerBaselineMocks } from "@/shared/api/mock";
import { i18n } from "@/shared/i18n";
import { toMs } from "@/shared/motion";
import { buildTheme, motion, radius, SPACING_SCALE, tokens, typography, type ThemeMode } from "@/theme";

const MODES: ThemeMode[] = ["dark", "light"];

/** 的时长预算：**不超过 250ms**（唯一例外是循环脉动，理由见 `theme.ts`）。 */
const MOTION_BUDGET_MS = 250;

/** MUI `styleOverrides` 是联合类型，测试里只读几个键，收窄一次即可。 */
type OverrideBag = Record<string, Record<string, unknown>>;

beforeAll(async () => {
  await i18n.changeLanguage("zh-CN");
});

afterEach(() => {
  cleanup();
});

describe("[15] 设计令牌：值与文档一致，且能被 MUI 正确消费", () => {
  it("中性色阶与 逐字一致（不用纯黑纯白）", () => {
    expect(tokens.palette.dark.bg).toEqual({
      base: "#0B0C0E",
      subtle: "#101114",
      surface: "#141619",
      raised: "#1A1D21",
    });
    expect(tokens.palette.dark.border).toEqual({
      subtle: "#1F2227",
      default: "#2A2E35",
      strong: "#3A3F47",
    });
    expect(tokens.palette.dark.text).toEqual({
      primary: "#E8EAED",
      secondary: "#9BA1A9",
      tertiary: "#6B7178",
      disabled: "#4A4F55",
    });

    expect(tokens.palette.light.bg).toEqual({
      base: "#FAFAFA",
      subtle: "#F4F4F5",
      surface: "#FFFFFF",
      raised: "#FFFFFF",
    });
    expect(tokens.palette.light.border).toEqual({
      subtle: "#EFEFF1",
      default: "#E1E2E4",
      strong: "#C9CBCF",
    });
    expect(tokens.palette.light.text).toEqual({
      primary: "#18191B",
      secondary: "#63666B",
      tertiary: "#8E9196",
      disabled: "#B4B7BB",
    });

    // 纯黑/纯白在 OLED 上有拖影、刺眼（约定）：窗口底与正文都不用它们
    // （浅色的 `bg.surface`/`raised` 按 表格就是 #FFFFFF，这里只管 base 与 text.primary）
    for (const mode of MODES) {
      const p = tokens.palette[mode];
      for (const value of [p.bg.base, p.text.primary]) {
        expect(["#000000", "#FFFFFF"]).not.toContain(value.toUpperCase());
      }
    }
  });

  it("强调色只有一个，语义色与 一致，且不是霓虹", () => {
    expect(tokens.palette.dark.accent).toBe("#5B5BD6");
    expect(tokens.palette.dark.accentHover).toBe("#6E6EE0");
    expect(tokens.palette.dark.success).toBe("#46B17B");
    expect(tokens.palette.dark.warning).toBe("#D2A24C");
    expect(tokens.palette.dark.danger).toBe("#E5605A");
    expect(tokens.palette.dark.idle).toBe("#6B7178");

    expect(tokens.palette.light.accent).toBe("#4F4FCF");
    expect(tokens.palette.light.accentHover).toBe("#4343C4");
    expect(tokens.palette.light.success).toBe("#2E8B62");
    expect(tokens.palette.light.warning).toBe("#9A7420");
    expect(tokens.palette.light.danger).toBe("#C0392F");
    expect(tokens.palette.light.idle).toBe("#8E9196");

    // 选中底色 = 强调色 12%（accent.subtle）
    expect(tokens.palette.dark.accentSubtle).toMatch(/0\.12/);
    expect(tokens.palette.light.accentSubtle).toMatch(/0\.12/);
  });

  it("间距刻度正好是那九个数；圆角只有三个值（）", () => {
    expect([...SPACING_SCALE]).toEqual([2, 4, 8, 12, 16, 24, 32, 48, 64]);
    expect(radius).toEqual({ control: 6, surface: 10, overlay: 14 });
  });

  it("字号只有六个、字重只有 400/500（）", () => {
    expect(typography.size).toEqual({ label: 11, meta: 12, body: 13, section: 15, page: 20, hero: 28 });
    expect(Object.values(typography.weight).sort()).toEqual([400, 500]);
    expect(typography.fontUI).toContain("Microsoft YaHei UI");
    expect(typography.fontMono).toContain("ui-monospace");
  });

  it("动效令牌：120/180 + 缓动曲线只有一条（）", () => {
    expect(motion.quick).toBe("120ms");
    expect(motion.enter).toBe("180ms");
    expect(motion.easing).toBe("cubic-bezier(.2,0,0,1)");

    // 缓动不许是关键字：只有 `cubic-bezier` 这种可精确表达的曲线才允许。
    expect(motion.easing).toContain("cubic-bezier");
  });

  it("动效预算：每个时长都 ≤250ms（机械检查 + 自证有牙）", () => {
    const overBudget = (bag: Record<string, string>): string[] =>
      Object.entries(bag)
        .flatMap(([name, token]) => {
          let ms: number;
          try {
            ms = toMs(token);
          } catch {
            return []; // 不是时长令牌（缓动曲线）
          }
          return ms > MOTION_BUDGET_MS ? [`${name}: ${token}`] : [];
        });

    expect(overBudget(motion)).toEqual([]);

    // 自证有牙：把预算违规的令牌喂进去必须被识出（否则这条检查只是"通过"而不是"守住"）
    expect(overBudget({ ...motion, quick: "300ms" })).toEqual(["quick: 300ms"]);
  });

  it("MUI 的 theme.transitions 也被收进令牌（默认的 300ms / easeInOut 是第二个时长来源）", () => {
    const allowed = new Set([toMs(motion.quick), toMs(motion.enter)]);
    const durationFindings = (theme: Theme): string[] =>
      Object.entries(theme.transitions.duration)
        .filter(([, value]) => !allowed.has(value) || value > MOTION_BUDGET_MS)
        .map(([name, value]) => `${name}: ${value}ms`);
    const easingFindings = (theme: Theme): string[] =>
      Object.entries(theme.transitions.easing)
        .filter(([, value]) => value !== motion.easing)
        .map(([name, value]) => `${name}: ${value}`);

    for (const mode of MODES) {
      const theme = buildTheme(mode);
      expect(durationFindings(theme), "所有档位都必须来自 motion 令牌").toEqual([]);
      expect(easingFindings(theme), "缓动只有 motion.easing 一条").toEqual([]);
    }

    // 自证有牙：MUI 的默认主题**必须**被判违规 —— 它正是我们要防的那份"第二个来源"
    // （`standard: 300ms` / `complex: 375ms` 超预算，`easeInOut` 不是我们的曲线）。
    const muiDefault = createTheme({});
    expect(durationFindings(muiDefault).length).toBeGreaterThan(0);
    expect(easingFindings(muiDefault).length).toBeGreaterThan(0);
  });

  it("buildTheme 把令牌映射进 MUI（背景台阶 / 分隔线 / 4px 间距 / 禁止 700）", () => {
    for (const mode of MODES) {
      const theme = buildTheme(mode);
      const p = tokens.palette[mode];

      // 层级靠背景明度台阶，不是靠加边框
      expect(theme.palette.background.default).toBe(p.bg.base);
      expect(theme.palette.background.paper).toBe(p.bg.surface);
      expect(theme.palette.divider).toBe(p.border.subtle);
      expect(theme.palette.primary.main).toBe(p.accent);
      expect(theme.palette.success.main).toBe(p.success);
      expect(theme.palette.warning.main).toBe(p.warning);
      expect(theme.palette.error.main).toBe(p.danger);

      // 间距基准 4px：刻度 ÷ 4 = sx 里的倍数
      // （cssVariables 主题下 MUI 产出 `calc(n * var(--mui-spacing, 4px))`，所以断言基准与倍数）
      expect(theme.spacing(4)).toContain("4 *");
      expect(theme.spacing(4)).toContain("4px");
      expect(theme.shape.borderRadius).toBe(radius.control);

      // 字重：MUI 的 "bold" 是 700，在深色底上会糊 —— 必须钳到 500
      expect(theme.typography.fontSize).toBe(typography.size.body);
      expect(theme.typography.fontWeightBold).toBe(500);
      expect(theme.typography.h1?.fontWeight).toBe(500);
      expect(theme.typography.body1?.fontSize).toBe(typography.size.body);

      // 圆角：面板 = radius.surface，浮层 = radius.overlay；阴影只给浮层
      const paper = theme.components?.MuiPaper?.styleOverrides as unknown as OverrideBag;
      const tooltip = theme.components?.MuiTooltip?.styleOverrides as unknown as OverrideBag;
      expect(paper["rounded"]?.["borderRadius"]).toBe(radius.surface);
      expect(tooltip["tooltip"]?.["borderRadius"]).toBe(radius.overlay);
      expect(tooltip["tooltip"]?.["boxShadow"]).toBe(p.shadowOverlay);
      expect(paper["root"]?.["boxShadow"]).toBe("none");
    }
  });

  it("核心组件在深/浅两态都能渲染", () => {
    for (const mode of MODES) {
      const view = render(
        <ThemeProvider theme={buildTheme(mode)}>
          <StatusDot status="ok" label="正常" />
          <NumberCard
            label="今日请求"
            value={1284}
            format="int"
            sparkline={[1, 2, 3]}
            delta={{ value: 0.032 }}
          />
        </ThemeProvider>,
      );

      expect(screen.getAllByText("正常").length).toBeGreaterThan(0);
      expect(screen.getByText("1,284")).toBeTruthy();
      // 涨跌用数字符号表达方向，不是箭头/方块字符
      expect(screen.getByText("+3.2%")).toBeTruthy();

      view.unmount();
    }
  });

  it("整个外壳在自定义主题下能渲染（mock 数据，不依赖 Rust/Tauri）", () => {
    // routes.test.tsx 用 MUI 默认主题渲染 AppShell；这里补的是**我们自己的主题**这一层：
    // 主题里的组件覆盖写错（比如 palette 路径不存在）只有真渲染才会显形。
    registerBaselineMocks();
    window.location.hash = "#/providers";
    const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
    render(
      <QueryClientProvider client={queryClient}>
        <App />
      </QueryClientProvider>,
    );

    expect(screen.getByText("TokenAgency")).toBeTruthy();
    // 这里**刻意不再断言某条路由是占位壳**：原先写的是 `/providers 还没实现 → 占位壳`，
    // 结果 /providers 一落地这条测试就红 —— 它把"某个页面还没做"编码进了测试里。
    // 页面会陆续落地，任何"必须是占位壳"的断言都会随之下一次变红。
    // 本用例要守的是"外壳 + 状态条在我们自己的主题下真的渲染出来了"，与哪条路由落地无关。
    // 状态条在自定义主题下把 mock 的"运行中"画出来了
    expect(screen.getAllByText("运行中").length).toBeGreaterThan(0);
  });
});
