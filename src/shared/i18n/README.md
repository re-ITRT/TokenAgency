# `src/shared/i18n` —— 文案与多语言

**只有两种语言**：`zh-CN`（默认）与 `en`。它们是 `index.ts` 里 `LOCALES` 的全部成员 ——
界面上那个"一键切换"就在这两者之间来回。

## 三个文件

| 文件 | 作用 |
| --- | --- |
| `zh-CN.ts` | 中文文案（**同时是 key 的类型真源**，见下） |
| `en.ts` | 英文文案（key 与中文**一一对应**，有守卫钉住） |
| `index.ts` | i18next 初始化、语言检测与切换、类型增强 |

## 机制（都在 `index.ts`）

* 用**内联资源**初始化，并设 `initImmediate: false` —— 同步完成，避免首帧 suspend；
* 语言检测顺序：`localStorage["ta.locale"]` → `navigator.language` 以 `zh` 开头取中文、
  否则取英文 → 非浏览器环境（测试 / SSR）取 `zh-CN`；
* `fallbackLng: "zh-CN"`；
* `setLocale()` 切换并持久化；`currentLocale()` 归一化成受支持的一种；`otherLocale()` 供切换按钮；
* **类型增强**：`declare module "i18next"` 把 `t()` 的 key 约束成 `typeof zhCN` ——
  写错 key 是**编译错误**，而不是等运行时才发现。

## 五条规矩

1. **用户可见文案不许写在组件里**。一律 `t("key")`，文案进 `zh-CN.ts` 与 `en.ts`。
2. **`zh-CN.ts` 是 key 的类型真源**。`en.ts` 少一个 key，类型**不会**报错 ——
   界面会**静默回落成中文**（`fallbackLng`）。所以有 `i18nParity.guard.test.ts` 钉住两边 key 一一对应。
3. **`i18nKey()` 是逃生舱**：它把 `string` 断成 `I18nKey`，等于**关掉类型检查**。
   只允许用在"key 由运行时算出、但必然在表里"的地方；
   **绝不允许**把用户输入或后端返回值塞给它再交给 `t()`。
4. **测试读真源、不抄中文**：断言里写 `zhCN["key"]`，不要把中文句子复制进测试 ——
   抄进去就变成"抄写测试"，改文案时两处都得改，还测不出文案是否真的渲染。
5. 界面上**不许出现编号引用与 emoji**：前者由 `uiMinimalism.guard` 管，后者由 `design.guard` 管。

## 守卫与工具

| 名字 | 管什么 |
| --- | --- |
| `src/app/i18nParity.guard.test.ts` | 两种语言的 key 必须一一对应（缺一个即红；带自检） |
| `src/app/uiMinimalism.guard.test.ts` | 版面文案长度、编号引用、英文侧同类词 |
| `src/app/design.guard.test.ts` | 设计系统与 emoji 零容忍 |
| `scripts/audit-inline-copy.mjs` | **只读审计**：列出铺在版面上、≥40 字的内联长文案（按页面分组）。它不是门禁，是"清理文案前先量一下"的工具 |

## 现状（实测）

两种语言各 **919 个 key，完全一一对应**（缺 0、多 0）。

## 加一种语言要动什么

1. `index.ts` 的 `LOCALES` 加一个成员；
2. 新建 `<locale>.ts`，key 与 `zh-CN.ts` 逐一对应；
3. `index.ts` 的 `resources` 里挂上它；
4. `detectLocale()` 的前缀判断补一条；
5. `README.md` 的语言栏加一条（README 的语言与界面语言保持同一套）。

`i18nParity.guard` 会**自动**把新语言纳入比对 —— 它从 `index.ts` 读 `LOCALES`，
不在这里另抄一份列表。
