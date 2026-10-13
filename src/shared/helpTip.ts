/**
 * 「区块说明放哪儿」的**唯一**判定（[docs/22 §2/§3]）。
 *
 * # 规矩
 *
 * 1. 说明**超过门槛**就收进标题右边的**问号圆圈**，短的（如「只读」「来自配置」）留在版面上；
 * 2. **一个区块最多一个问号**（§3）：一排问号比一排字更糟。所以同一区块还有别的长解释时，
 *    要**并进同一个问号**，而不是再画一个 —— 见 [`mergeHelpText`]；
 * 3. §2 的红线（读不到 / 未读取 / 校验失败 / 破坏性后果 / 数值）**一句都不许收**。
 *
 * # 为什么收敛到一处
 *
 * 第 319 轮机械测量发现这条规则被写了两遍，而且**阈值在两处各写了一份字面量**：
 *
 * | 位置 | 写法 |
 * |---|---|
 * | `network/index.tsx` | `const HELP_TIP_MIN_LENGTH = 12` + `length > HELP_TIP_MIN_LENGTH` |
 * | `settings/index.tsx` | `text.length > 12`（裸字面量） |
 *
 * 同一个数字出现在两处，改一处就会让两个页面的问号出现/消失的时机**不一致** ——
 * 而这种不一致**没有任何测试会红**（各页只测自己）。收敛之后阈值只有一份。
 */

/**
 * 超过多少个字符就收进问号圆圈。
 *
 * `12` 这个数来自 [docs/22 §2]：比它短的放进版面不会造成"一屏全是字"
 * （「只读」「来自配置」「直连」这类都在门槛之下）。
 */
export const HELP_TIP_MIN_LENGTH = 12;

/** 一个字符数够长的说明（**空**与 `undefined` 都不算 —— 它们既不渲染也不占位）。 */
export function collectIntoHelpTip(description: string | undefined): description is string {
  return (
    description !== undefined && description !== "" && description.length > HELP_TIP_MIN_LENGTH
  );
}

/** 说明的呈现方式：收进问号，还是留在版面上。 */
export function sectionHelpMode(text: string): "help" | "inline" {
  return text.length > HELP_TIP_MIN_LENGTH ? "help" : "inline";
}

/**
 * 把同一区块的几句解释合成**一条**提示正文（[docs/22 §3]：一个区块最多一个问号）。
 *
 * 两件事逼着要合并：提示气泡只吃一个字符串；而"一排问号"比一排字更糟。
 * 分隔符用**换行** —— `aria-label` 里保留断句，读屏不会把三句话连成一坨
 * （`HelpTip` 的气泡是 `whiteSpace: pre-line`）。
 *
 * 空串与 `undefined` 都被丢掉：多出来的空行会让 `aria-label` 里出现莫名其妙的停顿。
 */
export function mergeHelpText(...parts: readonly (string | undefined)[]): string {
  return parts.filter((part): part is string => part !== undefined && part !== "").join("\n");
}
