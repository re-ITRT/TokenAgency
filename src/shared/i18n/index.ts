/**
 * i18n 初始化。**中文是键集合的真源**（`zh-CN.ts`），英文由类型强制对齐。
 *
 * 默认语言：用户选过的优先；否则系统语言以 `zh` 开头给中文，其余给英文。
 * （桌面端产品说明与界面文案都以中文为准，英文是可选；判断逻辑集中在 `detectLocale()`。）
 *
 * 三个设计点：
 * 1. **资源内联 + `initImmediate: false` + 关 Suspense**：初始化是同步的，
 *    首帧就能渲染，不会出现"白屏等翻译"（DoD：每个路由都能渲染，不白屏）。
 * 2. **类型增强**：`t("nav.dashboard")` 的 key 由 `zhCN` 推导，
 *    写错 key 或漏写英文都会在 `tsc --noEmit` 阶段报错。
 * 3. 语言选择持久化在 `localStorage["ta.locale"]`，与后端无关（纯显示层）。
 */

import i18n from "i18next";
import { initReactI18next } from "react-i18next";

import { en } from "./en";
import { zhCN, type I18nKey } from "./zh-CN";

export type { I18nKey };

export const LOCALES = ["zh-CN", "en"] as const;
export type Locale = (typeof LOCALES)[number];

const STORAGE_KEY = "ta.locale";

/** 默认语言：用户选过就用选的；否则中文用户给中文，其它给英文。 */
function detectLocale(): Locale {
  if (typeof window === "undefined") return "zh-CN";
  const saved = window.localStorage.getItem(STORAGE_KEY);
  if (saved === "zh-CN" || saved === "en") return saved;
  const nav = window.navigator.language.toLowerCase();
  return nav.startsWith("zh") ? "zh-CN" : "en";
}

void i18n.use(initReactI18next).init({
  resources: {
    "zh-CN": { translation: zhCN },
    en: { translation: en },
  },
  lng: detectLocale(),
  fallbackLng: "zh-CN",
  supportedLngs: [...LOCALES],
  // 内联资源：同步完成初始化，避免首帧 suspend
  initImmediate: false,
  interpolation: { escapeValue: false },
  returnNull: false,
  react: { useSuspense: false },
});

/** 切换语言并持久化。设置页（F4）调这个。 */
export function setLocale(locale: Locale): void {
  if (typeof window !== "undefined") {
    window.localStorage.setItem(STORAGE_KEY, locale);
  }
  void i18n.changeLanguage(locale);
}

/** 当前语言（归一化成受支持的两种之一）。 */
export function currentLocale(): Locale {
  return i18n.resolvedLanguage === "en" ? "en" : "zh-CN";
}

/** 另一种语言（给"一键切换"按钮用）。 */
export function otherLocale(): Locale {
  return currentLocale() === "zh-CN" ? "en" : "zh-CN";
}

export { i18n };
export { useTranslation } from "react-i18next";

// 类型增强：让 t()/Trans 的 key 受 zh-CN 约束
declare module "i18next" {
  interface CustomTypeOptions {
    defaultNS: "translation";
    resources: { translation: typeof zhCN };
    returnNull: false;
  }
}
