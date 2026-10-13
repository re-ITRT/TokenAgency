/**
 * 视觉令牌 —— **[docs/15] 是本文件的唯一裁决处，这里不许出现文档之外的取值。**
 *
 * 规则（[15 §3]）：
 * - **颜色/间距/圆角只能从 `tokens` 取**，组件里不许写死颜色字面量、不许写刻度外间距；
 * - 每个值只有一处定义（深色 / 浅色是同构映射，不是两套设计）；
 * - 间距刻度只有九个：`2 4 8 12 16 24 32 48 64`；圆角只有三个；字号只有六个。
 *
 * 机械守卫：`src/app/design.guard.test.ts`（[15 §8]）会扫描 `src/**`，
 * 违反「无硬编码颜色 / 无刻度外间距 / 无 font-weight:700 / 无自造 emoji」即失败。
 * 也就是说：**本文件是唯一允许出现颜色字面量的地方。**
 */

import { alpha, createTheme, type Components, type Theme } from "@mui/material/styles";

import {
  COLOR_TRANSITION_PROPERTIES,
  MOTION_OFF_DURATION,
  MOTION_OFF_ITERATIONS,
  REDUCED_MOTION_QUERY,
  toMs,
} from "@/shared/motion";

export type ThemeMode = "dark" | "light";

// ================================================================ 中性色阶（[15 §3.1]）
/** 刻意不用蓝调的"开发者灰"，用真正中性的灰阶。层级靠背景明度台阶，不靠边框。 */
const NEUTRAL = {
  dark: {
    base: "#0B0C0E",
    subtle: "#101114",
    surface: "#141619",
    raised: "#1A1D21",
    borderSubtle: "#1F2227",
    borderDefault: "#2A2E35",
    borderStrong: "#3A3F47",
    textPrimary: "#E8EAED",
    textSecondary: "#9BA1A9",
    textTertiary: "#6B7178",
    textDisabled: "#4A4F55",
  },
  light: {
    base: "#FAFAFA",
    subtle: "#F4F4F5",
    surface: "#FFFFFF",
    raised: "#FFFFFF",
    borderSubtle: "#EFEFF1",
    borderDefault: "#E1E2E4",
    borderStrong: "#C9CBCF",
    textPrimary: "#18191B",
    textSecondary: "#63666B",
    textTertiary: "#8E9196",
    textDisabled: "#B4B7BB",
  },
} as const;

// ================================================================ 强调色与语义色（[15 §3.2]）
/** 低饱和，不用霓虹。强调色只有一个；语义色只表达状态，不做装饰。 */
const SEMANTIC = {
  dark: {
    accent: "#5B5BD6",
    accentHover: "#6E6EE0",
    success: "#46B17B",
    warning: "#D2A24C",
    danger: "#E5605A",
    idle: "#6B7178",
  },
  light: {
    accent: "#4F4FCF",
    accentHover: "#4343C4",
    success: "#2E8B62",
    warning: "#9A7420",
    danger: "#C0392F",
    idle: "#8E9196",
  },
} as const;

// ================================================================ 间距 / 圆角 / 排版 / 动效
/** [15 §3.3] 间距刻度 —— 只有这九个数（px）。 */
export const SPACING_SCALE = [2, 4, 8, 12, 16, 24, 32, 48, 64] as const;

/** [15 §3.3] 命名刻度。`sx` 里仍用 MUI 倍数（spacing=4），倍数只能是这里值的四分之一。 */
export const space = {
  xxs: 2,
  xs: 4,
  sm: 8,
  md: 12,
  lg: 16,
  xl: 24,
  xxl: 32,
  xxxl: 48,
  huge: 64,
} as const;

/** [15 §3.4] 圆角只有三个值。 */
export const radius = {
  /** 按钮、输入框、下拉、chip */
  control: 6,
  /** 卡片、面板、表格容器 */
  surface: 10,
  /** 菜单、模态、tooltip */
  overlay: 14,
} as const;

/** [15 §4] 排版。字重只用 400 / 500（不许 700）。 */
export const typography = {
  fontUI:
    '-apple-system, "Segoe UI Variable Text", "Segoe UI", "Microsoft YaHei UI", sans-serif',
  /** 数字必须等宽（tabular-nums），否则刷新时会左右跳动。 */
  fontMono: 'ui-monospace, "Cascadia Mono", "SF Mono", Consolas, monospace',
  size: { label: 11, meta: 12, body: 13, section: 15, page: 20, hero: 28 },
  weight: { regular: 400, medium: 500 },
  lineHeight: { body: 1.5, heading: 1.25, table: 1.2 },
  /** text.page 及以上略微收紧。 */
  letterSpacing: { large: "-0.01em", label: "0.06em" },
} as const;

/**
 * [15 §7] 动效：**时长只有一个来源（这里）**，且不超过 250ms。
 *
 * 组件里不许再出现 `transition: all .3s` 这类字面量（[26 §1.1]）；要过渡就用
 * `motion.enter` / `motion.quick` 配 `motion.easing`。MUI 自己的组件过渡也走这里 ——
 * `buildTheme` 会把 `theme.transitions` 映射成同一套值（MUI 默认的 300ms / `easeInOut`
 * 是**第二个时长来源**，而且超预算）。
 *
 * 机械守卫：`src/app/motion.test.ts`（扫源码）+ `theme.tokens.test.tsx`（扫令牌预算）。
 */
export const motion = {
  /** 状态变化（悬停、选中、出现即定） */
  quick: "120ms",
  /** 进出场（进场/展开、数值缓动） */
  enter: "180ms",
  /** 唯一的缓动曲线：快出缓入（[15 §7]） */
  easing: "cubic-bezier(.2,0,0,1)",
} as const;

/** [15 §3.5] 只有浮层用阴影，且只有一个层级；卡片默认零阴影。 */
const SHADOW = {
  dark: "0 8px 24px rgba(0,0,0,.45)",
  light: "0 8px 24px rgba(16,17,20,.12)",
} as const;

/**
 * 全部设计令牌。组件只从这里取（或经 `theme.palette` 取），
 * 因为 `tokens` 是唯一同时包含深/浅两态与几何值的入口。
 */
export const tokens = {
  spacing: SPACING_SCALE,
  space,
  radius,
  typography,
  motion,
  palette: {
    dark: {
      bg: {
        base: NEUTRAL.dark.base,
        subtle: NEUTRAL.dark.subtle,
        surface: NEUTRAL.dark.surface,
        raised: NEUTRAL.dark.raised,
      },
      border: {
        subtle: NEUTRAL.dark.borderSubtle,
        default: NEUTRAL.dark.borderDefault,
        strong: NEUTRAL.dark.borderStrong,
      },
      text: {
        primary: NEUTRAL.dark.textPrimary,
        secondary: NEUTRAL.dark.textSecondary,
        tertiary: NEUTRAL.dark.textTertiary,
        disabled: NEUTRAL.dark.textDisabled,
      },
      accent: SEMANTIC.dark.accent,
      accentHover: SEMANTIC.dark.accentHover,
      accentSubtle: alpha(SEMANTIC.dark.accent, 0.12),
      success: SEMANTIC.dark.success,
      warning: SEMANTIC.dark.warning,
      danger: SEMANTIC.dark.danger,
      idle: SEMANTIC.dark.idle,
      shadowOverlay: SHADOW.dark,
      onAccent: NEUTRAL.light.base,
    },
    light: {
      bg: {
        base: NEUTRAL.light.base,
        subtle: NEUTRAL.light.subtle,
        surface: NEUTRAL.light.surface,
        raised: NEUTRAL.light.raised,
      },
      border: {
        subtle: NEUTRAL.light.borderSubtle,
        default: NEUTRAL.light.borderDefault,
        strong: NEUTRAL.light.borderStrong,
      },
      text: {
        primary: NEUTRAL.light.textPrimary,
        secondary: NEUTRAL.light.textSecondary,
        tertiary: NEUTRAL.light.textTertiary,
        disabled: NEUTRAL.light.textDisabled,
      },
      accent: SEMANTIC.light.accent,
      accentHover: SEMANTIC.light.accentHover,
      accentSubtle: alpha(SEMANTIC.light.accent, 0.12),
      success: SEMANTIC.light.success,
      warning: SEMANTIC.light.warning,
      danger: SEMANTIC.light.danger,
      idle: SEMANTIC.light.idle,
      shadowOverlay: SHADOW.light,
      onAccent: NEUTRAL.light.surface,
    },
  },
} as const;

export type Tokens = typeof tokens;

/** MUI 组件 styleOverrides 的类型（只用于下面那个 severity 生成器，避免手写 12 个键）。 */
type AlertOverrides = NonNullable<Components<Theme>["MuiAlert"]>["styleOverrides"];

/** `filledError` / `standardWarning` … 的生成器：三种 severity、三种 variant 都是同一套语义色文字。 */
function severityOverride(
  variant: "filled" | "standard" | "outlined",
  severity: "Error" | "Warning" | "Success" | "Info",
  color: string,
): AlertOverrides {
  const name = `${variant}${severity}`;
  return {
    [name]: { color, "& .MuiAlert-message": { color }, "& .MuiAlert-icon": { color } },
  } as AlertOverrides;
}

/**
 * 扁平色板的结构类型（深/浅两态同构，所以返回类型不能是 `as const` 的字面量类型）。
 */
export type Palette = {
  bg: { base: string; subtle: string; surface: string; raised: string };
  border: { subtle: string; default: string; strong: string };
  text: { primary: string; secondary: string; tertiary: string; disabled: string };
  accent: string;
  accentHover: string;
  accentSubtle: string;
  success: string;
  warning: string;
  danger: string;
  idle: string;
  shadowOverlay: string;
  onAccent: string;
};

/** 当前模式的扁平色板（组件里需要 `tokens` 值又拿不到 theme 时用；优先 `theme.palette`）。 */
export function paletteOf(mode: ThemeMode): Palette {
  return tokens.palette[mode];
}

/**
 * 构建 MUI 主题：把 [15] 的令牌映射到 MUI 的调色板 / 排版 / 组件默认样式。
 *
 * 【为什么映射而不重定义】MUI 组件（按钮、表格、输入框、菜单…）内部走 `theme.palette`，
 * 如果我们只在组件里写 `sx`，MUI 自己的默认样式仍会是"通用后台"那套。所以令牌的
 * **唯一出口是这里**：映射错一次，全站一起错 —— 这正是我们要的（一致到像素）。
 */
export function buildTheme(mode: ThemeMode): Theme {
  const p = tokens.palette[mode];

  // 开启 CSS 主题变量：减少 Emotion 运行时样式重算（实时请求流高频更新时的关键）
  return createTheme({
    cssVariables: true,
    palette: {
      mode,
      primary: { main: p.accent, dark: p.accent, light: p.accentHover, contrastText: p.onAccent },
      secondary: { main: p.idle, contrastText: p.onAccent },
      success: { main: p.success, contrastText: p.onAccent },
      warning: { main: p.warning, contrastText: p.onAccent },
      error: { main: p.danger, contrastText: p.onAccent },
      info: { main: p.accent, contrastText: p.onAccent },
      text: {
        primary: p.text.primary,
        secondary: p.text.secondary,
        disabled: p.text.disabled,
      },
      background: { default: p.bg.base, paper: p.bg.surface },
      divider: p.border.subtle,
      action: {
        active: p.text.secondary,
        hover: p.bg.subtle,
        selected: p.accentSubtle,
        disabled: p.text.disabled,
        disabledBackground: p.bg.subtle,
      },
    },
    // spacing 基准 4px：sx 里的倍数只能是 [15 §3.3] 刻度 ÷ 4（守卫会拦刻度外的值）
    spacing: 4,
    shape: { borderRadius: radius.control },
    /**
     * MUI 自己的组件（按钮悬停、菜单、对话框、tooltip、表格行、tab…）内部全走这里。
     *
     * 不映射的话它们用 MUI 的默认值：`standard: 300ms` / `complex: 375ms` / `easeInOut` ——
     * 那就正是 [26 §1.1] 说的"第二个时长来源"，而且**超预算**（>250ms），
     * 还会在 reduced-motion 之外留下一条 `ease-in-out` 曲线。
     * 映射之后：进/出场用 `enter`、状态变化用 `quick`、缓动只有 `motion.easing` 一条。
     */
    transitions: {
      duration: {
        shortest: toMs(motion.quick),
        shorter: toMs(motion.quick),
        short: toMs(motion.enter),
        standard: toMs(motion.enter),
        // MUI 拿 `complex` 做多属性联动（默认 375ms 超预算）：它一样是因果过渡，没有理由更长
        complex: toMs(motion.enter),
        enteringScreen: toMs(motion.enter),
        // 出场比进场快（[15 §7] 快出缓入）：出场用 `quick`
        leavingScreen: toMs(motion.quick),
      },
      easing: {
        easeInOut: motion.easing,
        easeOut: motion.easing,
        easeIn: motion.easing,
        sharp: motion.easing,
      },
    },
    typography: {
      fontFamily: typography.fontUI,
      fontSize: typography.size.body,
      fontWeightRegular: typography.weight.regular,
      fontWeightMedium: typography.weight.medium,
      // 只用 400 / 500：MUI 的 "bold" 是 700，在深色底上会糊 —— 钳到 500
      fontWeightBold: typography.weight.medium,
      h1: {
        fontSize: typography.size.hero,
        fontWeight: typography.weight.medium,
        lineHeight: typography.lineHeight.heading,
        letterSpacing: typography.letterSpacing.large,
      },
      h2: {
        fontSize: typography.size.page,
        fontWeight: typography.weight.medium,
        lineHeight: typography.lineHeight.heading,
        letterSpacing: typography.letterSpacing.large,
      },
      h3: {
        fontSize: typography.size.page,
        fontWeight: typography.weight.medium,
        lineHeight: typography.lineHeight.heading,
        letterSpacing: typography.letterSpacing.large,
      },
      h4: {
        fontSize: typography.size.section,
        fontWeight: typography.weight.medium,
        lineHeight: typography.lineHeight.heading,
      },
      h5: {
        fontSize: typography.size.section,
        fontWeight: typography.weight.medium,
        lineHeight: typography.lineHeight.heading,
      },
      h6: {
        fontSize: typography.size.section,
        fontWeight: typography.weight.medium,
        lineHeight: typography.lineHeight.heading,
      },
      subtitle1: { fontSize: typography.size.body, fontWeight: typography.weight.medium },
      subtitle2: { fontSize: typography.size.body, fontWeight: typography.weight.medium },
      body1: { fontSize: typography.size.body, lineHeight: typography.lineHeight.body },
      body2: { fontSize: typography.size.body, lineHeight: typography.lineHeight.body },
      caption: { fontSize: typography.size.meta, lineHeight: typography.lineHeight.table },
      // 全大写小标签 / 表头：中文标签不做 text-transform（大小写对中文没意义）
      overline: {
        fontSize: typography.size.label,
        fontWeight: typography.weight.medium,
        letterSpacing: typography.letterSpacing.label,
        textTransform: "none",
        lineHeight: typography.lineHeight.table,
      },
      button: {
        fontSize: typography.size.body,
        fontWeight: typography.weight.medium,
        textTransform: "none",
      },
    },
    components: {
      MuiCssBaseline: {
        styleOverrides: {
          body: {
            backgroundColor: p.bg.base,
            color: p.text.primary,
            fontFamily: typography.fontUI,
            /*
             * [15 §7] / [26 §1.3] 必须遵守 prefers-reduced-motion —— 用 CSS 媒体查询，不用 JS：
             * JS 判断要等首帧之后才生效，那一帧的位移已经发生（闪一下正是要防的东西）。
             *
             * 两道闸，各管一件事：
             * ① **循环动画停掉**：所有动画只跑 0.01ms × 1 次 → 静止。
             *    属性级 + `!important`，所以**与样式表注入顺序无关** —— 这是真正生效的那道。
             * ② **位移/缩放/宽度/stroke 的过渡取消**，颜色过渡留下（颜色不引发前庭反应）。
             *    用 `transition-property` 而不是把时长压到 0.01ms：后者会把颜色反馈一起杀掉。
             */
            [REDUCED_MOTION_QUERY]: {
              "*, *::before, *::after": {
                animationDuration: `${MOTION_OFF_DURATION} !important`,
                animationIterationCount: `${MOTION_OFF_ITERATIONS} !important`,
                transitionProperty: `${COLOR_TRANSITION_PROPERTIES.join(", ")} !important`,
                scrollBehavior: "auto !important",
              },
            },
          },
          "::selection": { backgroundColor: p.accentSubtle },
        },
      },
      MuiPaper: {
        styleOverrides: {
          // 卡片默认零阴影（[15 §3.5]）；层级靠 bg 台阶 + border.subtle
          root: { backgroundImage: "none", boxShadow: "none" },
          // 面板/卡片用 radius.surface；菜单/模态/tooltip 在各自组件里用 radius.overlay
          rounded: { borderRadius: radius.surface },
          outlined: { borderColor: p.border.subtle },
        },
      },
      MuiButton: {
        defaultProps: { size: "small", disableElevation: true },
        styleOverrides: {
          root: { borderRadius: radius.control, paddingInline: space.md, minHeight: 28 },
          outlined: { borderColor: p.border.default },
        },
      },
      MuiIconButton: {
        defaultProps: { size: "small" },
        styleOverrides: { root: { borderRadius: radius.control } },
      },
      MuiTextField: { defaultProps: { size: "small" } },
      MuiOutlinedInput: {
        styleOverrides: {
          root: {
            borderRadius: radius.control,
            fontSize: typography.size.body,
            "& .MuiOutlinedInput-notchedOutline": { borderColor: p.border.default },
            "&:hover .MuiOutlinedInput-notchedOutline": { borderColor: p.border.strong },
          },
        },
      },
      MuiSelect: { defaultProps: { size: "small" } },
      // chip 不是状态色的载体（[15 §5] 状态用 6px 圆点 + 文字），这里只保证它不刺眼
      MuiChip: {
        defaultProps: { size: "small" },
        styleOverrides: {
          root: {
            borderRadius: radius.control,
            height: 22,
            fontSize: typography.size.meta,
            backgroundColor: "transparent",
            borderColor: p.border.default,
          },
        },
      },
      MuiDivider: { styleOverrides: { root: { borderColor: p.border.subtle } } },
      // 表格是本项目的 70%（[15 §5]）：行高 32、表头无底色、只用下边线、无竖线
      MuiTableCell: {
        styleOverrides: {
          root: {
            padding: "2px 8px",
            height: 32,
            fontSize: typography.size.body,
            lineHeight: typography.lineHeight.table,
            borderBottom: `1px solid ${p.border.subtle}`,
          },
          head: {
            fontSize: typography.size.label,
            fontWeight: typography.weight.medium,
            color: p.text.secondary,
            backgroundColor: "transparent",
            letterSpacing: typography.letterSpacing.label,
          },
        },
      },
      MuiTableRow: {
        styleOverrides: { root: { "&:hover": { backgroundColor: p.bg.subtle } } },
      },
      MuiListItemButton: {
        styleOverrides: {
          root: {
            borderRadius: radius.control,
            paddingInline: space.sm,
            minHeight: 32,
            "&.Mui-selected": {
              backgroundColor: p.accentSubtle,
              boxShadow: `inset 2px 0 0 0 ${p.accent}`,
              "&:hover": { backgroundColor: p.accentSubtle },
            },
          },
        },
      },
      MuiListItemText: { styleOverrides: { primary: { fontSize: typography.size.body } } },
      MuiSkeleton: { styleOverrides: { root: { backgroundColor: p.bg.subtle } } },
      // 浮层：唯一允许阴影的地方，且只有一个层级（[15 §3.5]）
      MuiTooltip: {
        defaultProps: { arrow: true },
        styleOverrides: {
          tooltip: {
            backgroundColor: p.bg.raised,
            color: p.text.primary,
            border: `1px solid ${p.border.subtle}`,
            borderRadius: radius.overlay,
            boxShadow: p.shadowOverlay,
            fontSize: typography.size.meta,
            padding: `${space.xs}px ${space.sm}px`,
            maxWidth: 360,
          },
          arrow: { color: p.bg.raised },
        },
      },
      MuiMenu: {
        styleOverrides: {
          paper: {
            backgroundColor: p.bg.raised,
            border: `1px solid ${p.border.subtle}`,
            borderRadius: radius.overlay,
            boxShadow: p.shadowOverlay,
          },
          list: { padding: `${space.xs}px 0` },
        },
      },
      MuiMenuItem: {
        styleOverrides: {
          root: {
            fontSize: typography.size.body,
            minHeight: 32,
            padding: `${space.xs}px ${space.md}px`,
          },
        },
      },
      MuiPopover: {
        styleOverrides: {
          paper: {
            backgroundColor: p.bg.raised,
            border: `1px solid ${p.border.subtle}`,
            borderRadius: radius.overlay,
            boxShadow: p.shadowOverlay,
          },
        },
      },
      MuiDialog: {
        styleOverrides: {
          paper: {
            backgroundColor: p.bg.raised,
            border: `1px solid ${p.border.subtle}`,
            borderRadius: radius.overlay,
            boxShadow: p.shadowOverlay,
          },
        },
      },
      // [15 §2.1] 失败 = 文字 + danger 色文字；不要红底白字的大色块。
      // 所以三种 severity 一律：浮层底色 + 语义色**文字**（不是色块）。
      MuiAlert: {
        styleOverrides: {
          root: {
            borderRadius: radius.control,
            border: `1px solid ${p.border.subtle}`,
            backgroundColor: p.bg.raised,
            color: p.text.primary,
            fontSize: typography.size.body,
            padding: `${space.sm}px ${space.md}px`,
            alignItems: "center",
          },
          message: { padding: 0, width: "100%" },
          icon: { padding: 0, marginRight: space.sm, opacity: 1 },
          ...severityOverride("filled", "Error", p.danger),
          ...severityOverride("filled", "Warning", p.warning),
          ...severityOverride("filled", "Success", p.success),
          ...severityOverride("filled", "Info", p.accent),
          ...severityOverride("standard", "Error", p.danger),
          ...severityOverride("standard", "Warning", p.warning),
          ...severityOverride("standard", "Success", p.success),
          ...severityOverride("standard", "Info", p.accent),
          ...severityOverride("outlined", "Error", p.danger),
          ...severityOverride("outlined", "Warning", p.warning),
          ...severityOverride("outlined", "Success", p.success),
          ...severityOverride("outlined", "Info", p.accent),
        },
      },
      MuiLinearProgress: {
        styleOverrides: {
          root: { height: 6, borderRadius: radius.control, backgroundColor: p.bg.subtle },
          bar: { backgroundColor: p.accent },
        },
      },
      MuiCircularProgress: { defaultProps: { color: "inherit" } },
      MuiTabs: { styleOverrides: { root: { minHeight: 32 } } },
      MuiTab: {
        styleOverrides: {
          root: {
            minHeight: 32,
            fontSize: typography.size.body,
            textTransform: "none",
            fontWeight: typography.weight.regular,
          },
        },
      },
      MuiLink: {
        defaultProps: { underline: "hover" },
        styleOverrides: { root: { color: p.accent } },
      },
    },
  });
}
