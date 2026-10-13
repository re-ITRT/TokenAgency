/**
 * 「后端原始对象 → 视图字段」的**唯一**读取件。
 *
 * # 为什么收敛到一处
 *
 * 第 319 轮机械测量发现：`asRow` / `pickStr` / `pickNum` / `pickBool` 这**四个函数
 * 在 5 个页面 model 里各写了一遍**（`settings` / `logs` / `network` / `providers/model` /
 * `providers/detail` 用单键版），合计 15 份定义：
 *
 * | 形状 | 份数 |
 * |---|---|
 * | `asRow` | 5 |
 * | `pickStr` | 5 |
 * | `pickNum` | 5 |
 * | `pickBool` | 4 |
 *
 * 而它们**已经漂了**：`network` / `settings` 的 `asRow` 把数组当对象收，
 * `providers` / `editor` / `detail` 的则把数组挡掉。同一个函数名、两种语义 ——
 * 这正是 [docs/50 §2] 那种"重复必然漂移"的样本。
 *
 * # 统一到哪一边：**挡掉数组**
 *
 * 数组不是"一行"。`asRow([])` 收下数组之后，`row["model"]` 照样是 `undefined`，
 * 所以对既有调用点**行为等价**；但把数组挡掉能让"拿一整个列表当一行用"这种错
 * **更早暴露**（例如 `Object.keys([]).length === 0` 会被误读成"这一行是空的"）。
 *
 * # 纪律
 *
 * * **不猜默认值**：读不到就是 `undefined`，调用方自己决定画「—」还是留空
 *   （[docs/22 §2] 的红线：`—` 表示读不到，`0` 表示确实是 0）；
 * * `pickStr` 把**空串**也当读不到（空串在本仓一律表示"没填/没给"，见 `optional()`）；
 * * `pickNum` 只收**有限**数（`NaN` / `Infinity` 是坏数据，不是 0）；
 * * 多键版（`pickStr(row, "a", "b")`）按给定顺序取**第一个**存在的，
 *   用于"同一个事实在不同版本里键名不同"（例如 `input_cached` 与 `cached`）。
 */

/** 反序列化后的未知对象：键名是后端的事，所以按 `Record` 处理并逐条注明来源。 */
export type Row = Record<string, unknown>;

/**
 * 把任意值收窄成可安全取值的一层对象。
 *
 * 不是普通对象（`null` / 数组 / 字符串 / 数字…）一律给空对象 ——
 * 于是调用点可以无条件 `row["x"]`，不需要每个地方都写一遍判空。
 */
export function asRow(value: unknown): Row {
  return typeof value === "object" && value !== null && !Array.isArray(value)
    ? (value as Row)
    : {};
}

/**
 * 键**存在**（哪怕值是 `null`）。
 *
 * 用来区分「后端没给这个键」与「后端明确给了 `null`」——
 * 前者是"未读取"，后者是"未指定"，两者在界面上说的话不一样。
 */
export function hasKey(row: Row, key: string): boolean {
  return Object.prototype.hasOwnProperty.call(row, key);
}

/** 第一个非空字符串（空串 = 读不到；`undefined` = 一个都没有）。 */
export function pickStr(row: Row, ...keys: string[]): string | undefined {
  for (const key of keys) {
    const value = row[key];
    if (typeof value === "string" && value !== "") return value;
  }
  return undefined;
}

/** 第一个有限数字（`NaN` / `Infinity` / 非数字一律跳过）。 */
export function pickNum(row: Row, ...keys: string[]): number | undefined {
  for (const key of keys) {
    const value = row[key];
    if (typeof value === "number" && Number.isFinite(value)) return value;
  }
  return undefined;
}

/** 第一个布尔值（**不**把 `0` / `"true"` 之类当布尔 —— 那是在替后端解释数据）。 */
export function pickBool(row: Row, ...keys: string[]): boolean | undefined {
  for (const key of keys) {
    const value = row[key];
    if (typeof value === "boolean") return value;
  }
  return undefined;
}

/** 这个键上的数组（不是数组就是 `undefined` —— **不**把空数组与读不到混为一谈）。 */
export function pickArr(row: Row, key: string): unknown[] | undefined {
  const value = row[key];
  return Array.isArray(value) ? value : undefined;
}

/**
 * 把这个键上的数组逐项过一遍 `asRow`。
 *
 * 存在的理由：`(raw["rows"] ?? []).map(asRow)` 这种写法在 5 个 model 里反复出现，
 * 而手写的版本有两种错法 —— 忘了 `?? []`（`undefined.map` 直接抛）、
 * 或忘了逐项收窄（后面每一项都要再判一次类型）。
 */
export function pickRows(row: Row, key: string): Row[] {
  const value = pickArr(row, key);
  return value === undefined ? [] : value.map((item) => asRow(item));
}
