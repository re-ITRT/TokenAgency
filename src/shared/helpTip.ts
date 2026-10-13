/**
 * 「区块说明放哪儿」的**唯一**判定。
 *
 * 1. 说明**超过门槛**就收进标题右边的**问号圆圈**，短的（「只读」「来自配置」）留在版面上；
 * 2. **一个区块最多一个问号**：一排问号比一排字更糟。同一区块还有别的长解释时，
 *    要**并进同一个问号**，而不是再画一个 —— 见 [`mergeHelpText`]；
 * 3. 红线（读不到 / 未读取 / 校验失败 / 破坏性后果 / 数值）**一句都不许收**。
 *
 * 阈值只准有这一份：它曾被复制成两处裸字面量（一处常量、一处 `length > 12`），
 * 两个页面的问号出现时机因此不一致，而**各页只测自己，没有任何测试会红**。
 */

/**
 * 超过多少个字符就收进问号圆圈。
 *
 * `12` 是"放进版面不会造成一屏全是字"的门槛：「只读」「来自配置」「直连」都在它之下。
 */
export const HELP_TIP_MIN_LENGTH = 12;

/** 够长的说明才收（**空**与 `undefined` 都不算 —— 它们既不渲染也不占位）。 */
export function collectIntoHelpTip(description: string | undefined): description is string {
  return (
    description !== undefined && description !== "" && description.length > HELP_TIP_MIN_LENGTH
  );
}

export function sectionHelpMode(text: string): "help" | "inline" {
  return text.length > HELP_TIP_MIN_LENGTH ? "help" : "inline";
}

/**
 * 把同一区块的几句解释合成**一条**（一个区块最多一个问号）。
 *
 * 分隔符用**换行** —— 气泡是 `whiteSpace: pre-line`，`aria-label` 靠它断句，
 * 读屏不会把三句话连成一坨。空串要丢掉，否则 `aria-label` 里会多出停顿。
 */
export function mergeHelpText(...parts: readonly (string | undefined)[]): string {
  return parts.filter((part): part is string => part !== undefined && part !== "").join("\n");
}
