# 18 · 订阅流量可见 + 网络代理选装（第 62 轮主管定稿）

> 两条都来自用户原话：
> ①「clash 的面板能看到**更新时间** 2026-09-21 21:44、**已使用/总量** 92.9GB/130GB 71%，我希望我们也能看到」
> ②「我们的网络代理模块也需要设置，**如果用户没有配置就不显示**，token 网络代理是一个**选装功能**」

## 1. 订阅流量与更新时间（等价于 Clash 面板那一行）

### 1.1 数据来源（现在完全没有）
订阅服务商按 Clash 生态的约定，在 **HTTP 响应头 `subscription-userinfo`** 里给流量信息：

```
subscription-userinfo: upload=123456789; download=98765432100; total=139586437120; expire=1893456000
```

- 单位是**字节**；`expire` 是 **Unix 秒**（`0` 或缺失 = 不适用/未知）。
- 现在全仓**没有任何地方读它**（已核：`crates/**` 里只有一处测试提到 URL 的 userinfo 语法）。
- 解析必须在**拉取订阅的那一处**做（`crates/ta-subscription` 的 fetch 路径），因为它只有在那时可得。

### 1.2 契约（最小形状，三处同步：`docs/10` + `src/shared/bindings.ts` + Rust）
```ts
/** 订阅服务商在响应头里给的流量信息。**全部用「未知 = null」，绝不用 0 冒充。** */
export type SubscriptionTraffic = {
  upload_bytes: number | null;
  download_bytes: number | null;
  total_bytes: number | null;   // 缺失/NaN → null
  expire_at_s: number | null;   // Unix 秒；0 → 视为 null（不适用）
  fetched_at_ms: number;        // 这一次拿到的时刻（必填，用来显示「更新时间」）
};
```
- `RefreshResult` 增加 `traffic?: SubscriptionTraffic | null`（刷新成功时带回；没给头就是 `null`）。
- **列表要能显示"上次已知"**，所以必须**持久化**：在 `subscription` 表上新增列
  （`upload_bytes` / `download_bytes` / `total_bytes` / `expire_at_s` / `traffic_at_ms`，全部可空），
  并提供读取路径（二选一，实现者定，但要在 `docs/10` 写清）：
  - (a) 新增只读命令 `subscription_traffic`（返回 `{ id, traffic }[]`）；或
  - (b) 把 `subscription_list` 的返回类型改成带 `traffic: SubscriptionTraffic | null` 的行类型。
  **推荐 (a)**：`Subscription` 是配置实体，流量是运行时观测值，混在一起会污染配置。

### 1.3 界面（`/network` 的订阅行）
显示三样，缺哪样就**不显示那一样**（不显示 `0`、不显示占位符）：
1. **更新时间**：由 `fetched_at_ms` 按本地时区格式化成 `YYYY-MM-DD HH:mm`（Clash 面板同款）。
2. **已用/总量**：`used = upload + download`（两者都有才算）；`total` 有就显示 `92.9 GB / 130 GB`。
   单位用 GB（二进制 1024³）并保留一位小数，与 Clash 面板一致。
3. **百分比**：`used / total`，保留整数（`71%`）；`total` 缺失或为 0 → 不显示百分比。
- 三个字段都来自后端；**前端不自己算 total、不把缺失当 0**。
- 若后端返回 `traffic: null`（该订阅从没成功刷新过），行上**只显示"尚未获取"**（一句说明），不显示数字。

## 2. 网络代理是**选装**功能：没配置就不显示

### 2.1 「未配置」的判定（唯一口径，Rust 与界面一致）
```
未启用 = net.kernel.path 为空  且  订阅列表为空  且  线路（lane）列表为空
```
（内核路径、订阅、线路三者都没有 = 用户没在用这块功能。）

### 2.2 界面行为（`/network`）
- **未启用时**：**不渲染**订阅表、线路表、分配表、内核状态块 —— 一个空表、一个"暂无数据"、一个禁用控件都不要。
  只渲染**一个**块：标题「网络代理（可选，未启用）」+ 一句说明（这块功能是选装，用于让请求走代理/机场线路）
  + **一个明确的启用入口**（按钮/引导：去设置内核路径、或添加第一条订阅）。
- **一旦有任一项**（内核路径/订阅/线路）：才渲染对应的区块；未涉及的区块仍不渲染空表。
- 其它页面不受影响；网络未启用时内核相关命令如实报错（已有 TA-1004），界面不许把错误说成"已停止"。

### 2.3 侧边栏
网络入口**保留**（否则用户找不到启用入口），但未启用时**不带任何红点/错误态**——它是可选功能，不是异常。

## 3. 验收
1. **红先绿后**：解析函数（有头/无头/字段缺失/非法值）、持久化读回、界面三条显示规则各有用例；
   "未配置 → 不渲染空表"要有**反向断言**（未配置时 `network-subscriptions` / `network-lanes` 等 testid **不存在**）。
2. 契约三处同步（`docs/10` / `bindings.ts` / Rust），`command_surface_guard` 绿。
3. 真机 E2E：`scripts/e2e/smoke.mjs` 增加一步——
   - 用本地 fixture 返回带 `subscription-userinfo` 的订阅 → 断言界面出现"更新时间 + 已用/总量 + 百分比"，
     且数字与 fixture 里的字节数**对得上**（92.9GB/130GB 这类格式）；
   - 再断言"未配置网络时网络区块不存在"（用一个干净的 scratch 数据目录启动即可）。
