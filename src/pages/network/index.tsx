/**
 * 网络页（[04 §5–§8]、[12 §6 F4]、[19 §2]）—— `/network` 概览，**以及三个网络视图共用的展示件**。
 *
 * ## 三条硬纪律
 *
 * 1. **I2：系统网络零副作用。** 这一页（含节点列表与线路分配）**不碰系统网络**：
 *    没有"改系统代理 / DNS / 路由 / 网卡 / 开机自启"的路径或按钮，也没有启停网关或内核的按钮。
 *    页面调用的命令是一条可机械检查的白名单（见 `src/app/network.test.tsx` 的命令面守卫）：
 *    只读的 `app_status` / `subscription_list` / `lane_list` / `channel_matrix` / `config_get`，
 *    外加**只管订阅与线路这两个配置实体**的写命令（`subscription_*` / `lane_upsert` /
 *    `lane_delete` / `lane_set_node`）与**不改配置**的逐节点测速 `lane_test`
 *    —— 后者改的是 TokenAgency 自己的配置与内核运行态，系统网络配置一个字段都不动。
 *    守卫对"系统/内核/托盘写命令"仍然零容忍。
 * 2. **不猜。** 读不到的字段显示「未读取」，配置里确实是空的显示空状态，两者**不是一回事**；
 *    写操作**只显示后端返回的对象**（不拼一句"保存成功"），失败原样显示 TA 码。
 *    需要但尚未注册的后端接口如实点名，而不是编一份看起来完整的节点清单
 *    （见 `model.ts` 文件头「一」的三态区分）。
 * 3. **常显（[19 §2.2]，取代 [18 §2.2] 的"选装入口"）。** 网络代理**默认开启、默认直连**：
 *    这一页永远渲染出口 / 订阅 / 线路，没配置时给诚实的空态 —— 页面上**没有"启用网络代理"这个动作**。
 *    未配内核路径时出口显示「出口 · 直连」（`network.directLabel`），不是错误态、不点红点。
 *
 * ## 写界面与读界面为什么在同一页
 *
 * 订阅与线路是"看一眼、改一下、再看一眼"的闭环：列表里的每一行都带刷新 / 删除 / 测速，
 * 表单就在列表下面。**只读的五级链路解析**（后端算的最终生效线路）放在 `/network/assignments`，
 * 因为"删 lane 之前先看谁在引用它"是另一个操作流。
 * 写操作走 `useCommandMutation` + `invalidateQueries`：**不用本地状态伪造一行结果**。
 *
 * ## 删除（[docs/21 §1]，第 73 轮补）
 *
 * 订阅与线路的删除**都必须先二次确认**（共享组件 `ConfirmDelete`），且这两条命令**没有 `force` 语义**
 * → 被后端拒绝时只把原文（含 TA 码）摆出来，**不放**"强行删除"按钮（那会是假按钮）。
 * 单条与批量**共用同一个确认组件与同一套失败展示**；批量是"一次确认 → 逐条调用 → 逐条汇报"，
 * 一条失败不中止其余条，也绝不用一句"批量删除成功"盖住部分失败（[docs/21 §1.2]）。
 * 删除成功后要失效的 key 见两个区块各自的 `invalidateAfterDelete`（订阅连带线路与线路下拉；
 * 线路连带通道矩阵 —— 矩阵的 key 不在 `qk.lanes` 前缀里，漏了它矩阵就会停在旧值上）。
 *
 * ## 为什么这个文件还导出别的视图要用的组件
 *
 * `pageRegistry` 把 `src/pages/**` 下**每一个 `.tsx`** 都当成一个页面候选，而
 * `noPlaceholder.guard.test.ts` 会断言"每个页面文件都在路由表里有归属（不许有孤儿页面）"。
 * 路由表（`routes.ts`）给 network 的候选只有 `network` / `network/index` /
 * `network/nodes` / `network/assignments` 四条。所以共用展示件**不能**单独放一个
 * `sections.tsx` —— 那会被判成孤儿页面并把守卫变红。三个视图共用的这几个小组件
 * 因此挂在本文件（`/network` 的页面模块）上，由 `./nodes` 与 `./assignments` import。
 */

import {
  Box,
  Button,
  Checkbox,
  Dialog,
  DialogActions,
  DialogContent,
  DialogTitle,
  Divider,
  FormControlLabel,
  IconButton,
  Menu,
  MenuItem,
  Paper,
  Stack,
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableRow,
  TextField,
  Typography,
} from "@mui/material";
import MoreVertIcon from "@mui/icons-material/MoreVert";
import { useQueryClient } from "@tanstack/react-query";
import { useState, type ReactNode } from "react";
import { Link, useSearchParams } from "react-router-dom";

import { errorMessage, qk, useAppStatus, useCommandMutation, useCommandQuery } from "@/shared/api";
import type { AppStatus } from "@/shared/bindings";
import {
  AutoEllipsis,
  Caption,
  ConfirmDelete,
  HelpTip,
  KernelStateDot,
  PageHeader,
  StatusDot,
} from "@/shared/components";
import {
  formatAgo,
  formatBytes,
  formatDuration,
  formatInt,
  formatLatency,
  formatLocalTime,
} from "@/shared/format";
import { useTranslation } from "@/shared/i18n";
import { radius, typography } from "@/theme";

import {
  DASH,
  HEALTH_LABEL,
  HEALTH_STATUS,
  SUBSCRIPTION_FORMATS,
  isKnownHealth,
  laneKindLabel,
  laneNameOf,
  parseOptionalCount,
  readKernelPath,
  readLane,
  readSubscription,
  subscriptionDraftComplete,
  subscriptionDraftFromRow,
  subscriptionEditPayload,
  subscriptionFormatLabel,
  subscriptionPayload,
  summarizeTraffic,
  type LaneRow,
  type SubscriptionDraft,
  type SubscriptionRow,
  type TrafficRow,
} from "./model";

// ================================================================ 共用展示件（见文件头末节）

/**
 * 这里原有本页自己的 `HELP_TIP_MIN_LENGTH` / `mergeHelpText` / `collectIntoHelpTip` 三件。
 *
 * **第 319 轮删除**（[docs/52]）：同一套规则在 `settings/index.tsx` 又写了一遍，
 * 而且阈值在那处是一个**裸字面量 `12`** —— 改一处就会让两个页面的问号出现时机不一致，
 * 且没有任何测试会红。现在统一走 `@/shared/helpTip`。
 *
 * `mergeHelpText` 仍然从本文件**再导出**：`./assignments` / `./nodes` 两个兄弟模块
 * 是从 `./index` 取它的，直接删掉会让它们编译失败
 * （与 `DASH` 那次同一个教训：删导出必须让编译器过一遍）。
 * 新代码请直接 `import { mergeHelpText } from "@/shared/helpTip"`。
 */
import { collectIntoHelpTip, mergeHelpText } from "@/shared/helpTip";

// 兄弟模块（`./assignments` / `./nodes`）是从 `./index` 取 `mergeHelpText` 的，
// 所以这里保留一次再导出（新代码请直接从 `@/shared/helpTip` 取）。
export { mergeHelpText };

/** 契约出处标注、说明文字：一律 `caption` + 次要色，不抢正文。 */

/**
 * 一个只读区块：标题 + 说明 + 内容。**不是一个装饰性卡片**（无阴影、无强调色）。
 *
 * 说明的去处由长度决定（[docs/22 §2]）：超过 {@link HELP_TIP_MIN_LENGTH} 个字符的收进标题右边的
 * 问号圆圈（正文进 tooltip + `aria-label`，一个字都不丢），短的留在原位。
 * **没有 `description` 时不留空行/占位**（`description` 为 `undefined` 或空串都算没有）。
 * `children` 可省：纯说明区块（把整页的边界讲清楚、没有数据要摆）收完之后就只剩标题 + 问号。
 *
 * **第 326 轮删掉了 `hint` / `hintTestId` 两个 prop**：它们把第二段解释画在标题**下面单独一行**，
 * 于是区块里出现一个**悬在工具栏上方、没有任何标签**的问号圆圈（用户真机截图里的那个孤儿 `?`，
 * 谁也不知道它在问什么）。同一个区块本来就只该有一个问号（[docs/22 §3]），
 * 所以现在只有一条路：把要解释的话并进 `description`（用
 * {@link mergeHelpText}），由它一处决定收进问号还是留在版面上。
 */
export function SectionPaper({
  title,
  description,
  help,
  testId,
  children,
}: {
  title: string;
  description?: string;
  /**
   * **已经渲染好的问号**（`<HelpTip … />`）。
   *
   * 为什么需要这个口子：本区块的说明常常是**多键合并**出来的
   * （`mergeHelpText(t("a"), t("b"), …)`）。极简守卫（`uiMinimalism.guard.test.ts` 规则 2）
   * 只把**字面量就写在 `<HelpTip … />` 里**的键当成按需展开的说明，多键合并出来的长句会被它
   * **保守地**误算成"版面文案"（守卫自己的注释也承认这一点）。由调用方把 `<HelpTip>` 直接写出来，
   * 守卫看到的就是事实：这几句确实只在问号里。
   *
   * 传了 `help` 就不再按 `description` 自动生成问号（[docs/22 §3]：一个区块只有一个问号）。
   */
  help?: ReactNode;
  testId?: string;
  children?: ReactNode;
}) {
  const asHelpTip = help === undefined && collectIntoHelpTip(description);

  return (
    <Paper variant="outlined" sx={{ p: 4, borderRadius: `${radius.surface}px` }} data-testid={testId}>
      <Stack spacing={3}>
        <Box>
          {/* 问号圆圈与标题同一行（与页头同一个排布）；短说明仍走下一行的 `caption` */}
          <Box sx={{ display: "flex", alignItems: "center", gap: 2, minWidth: 0 }}>
            <Typography variant="h4">{title}</Typography>
            {help !== undefined ? (
              help
            ) : asHelpTip ? (
              <HelpTip
                text={description}
                testId={testId === undefined ? undefined : `${testId}-help`}
              />
            ) : null}
          </Box>
          {help !== undefined ||
          asHelpTip ||
          description === undefined ||
          description === "" ? null : (
            <Caption>{description}</Caption>
          )}
        </Box>
        {children}
      </Stack>
    </Paper>
  );
}

/**
 * 这里原有本页自己的 `StatItem`。
 *
 * **第 319 轮删除**（[docs/52]）：它与 `settings` / `logs` / `models` 三份是同一样东西，
 * 已收敛到 `@/shared/components` 的 `StatItem`。本页自己不再用它
 * （`assignments` / `nodes` 两个兄弟模块也从未 import 过它），所以直接删。
 */

/**
 * I2 的**用户可见面**：把"这一页不碰系统网络"写在页面上，而不是只写在文档里。
 *
 * **第 326 轮：整张卡片删掉了。** 它占着一整块版面，只为了说一句 `只展示，不改系统网络`
 * 加四个否定句（用户真机截图里的反例：*"整张卡片只为了说…"*）。这条承诺**一个字都没少**，
 * 只是换了去处：两个页面各自用 `mergeHelpText` 把 `network.zeroSideEffect.title` /
 * `network.zeroSideEffect.body` 接在页头 `description` 的后面 —— 也就是标题旁那个问号里。
 *
 * 为什么是搬走而不是删掉：网络类工具的用户最怕的就是"我点开一个页面，它顺手把我系统代理改了"。
 * 这是承诺，不能消失 —— 但也不必天天摊在脸上占半屏。
 */

/** 线路健康：6px 圆点 + 文字（[15 §5] 不许用彩色 chip）。 */
export function HealthCell({ health, raw }: { health: LaneRow["health"]; raw?: string }) {
  const { t } = useTranslation();
  // 契约外的取值原样显示（是后端说的话，不改写）；我们认识的词表才翻成中文
  const label = raw !== undefined && !isKnownHealth(raw) ? raw : t(HEALTH_LABEL[health]);
  return <StatusDot status={HEALTH_STATUS[health]} label={label} />;
}

/**
 * 「未读取」的统一写法（**不是**空值、**不是** 0）。
 *
 * 固定 `component="span"`：它会出现在表格单元格里，也会出现在别的 `<Typography>`（即 `<p>`）
 * 内部 —— 用默认的 `<p>` 会得到非法的 `<p><p>` 嵌套（React 会告警，浏览器会重排）。
 */
export function Unreadable({ testId }: { testId?: string }) {
  const { t } = useTranslation();
  return (
    <Typography
      variant="body1"
      component="span"
      data-testid={testId}
      sx={{ color: "text.secondary" }}
    >
      {t("network.unreadable")}
    </Typography>
  );
}

// 上一版这里有一个 `MissingCommandNote`（"后端这个接口还没注册"的统一说明）与一个
// `OptionalNetworkBlock`（未配置时唯一的"启用网络代理"入口）。两者都随 [19 §2] 作废：
// - 节点清单 / 订阅解析 / 逐节点测速三条命令**都已接入**，那些"本页尚未接入"的说明是**过期说明**；
// - 网络代理默认开启（默认直连），页面上**没有"启用网络代理"这个动作**。
// 于是这一页不再有任何"这条命令还没注册"的占位文案 —— 真缺接口时再按需加回来。

// ================================================================ ⓪ 从服务商向导返回（[19 §1.4]）
//
// 服务商两步向导的「让我先设置网络代理」把用户送到 `#/network?from=provider-wizard`，
// 于是这一页顶部要给一条**回得去**的路：文案说清"正在为服务商配置网络代理"，
// 按钮回 `/providers`（向导的草稿存在页面级 store 里，回去还在）。
// 触发条件只看 URL 参数：不是"任何从别处进来的链接都弹一条返回条"。

export function ProviderReturnBanner() {
  const { t } = useTranslation();
  return (
    <Paper
      variant="outlined"
      data-testid="network-return-banner"
      sx={{ p: 4, borderRadius: `${radius.surface}px` }}
    >
      <Stack direction="row" spacing={3} alignItems="center" sx={{ flexWrap: "wrap" }}>
        <Typography variant="body1">{t("network.returnBanner")}</Typography>
        {/* MUI 控件不吃 `data-testid`（要取到真正的按钮得再往下一层）→ 外层普通元素包住它 */}
        <Box data-testid="network-return-btn">
          <Button variant="outlined" component={Link} to="/providers">
            {t("network.returnBtn")}
          </Button>
        </Box>
      </Stack>
    </Paper>
  );
}

const TABLE_WRAP_SX = { overflowX: "auto" } as const;

/**
 * **单元格一律不换行**（规则 4）。
 *
 * 列数砍到 ≤6 之后宽度本来就够；长值（订阅地址、节点名）走 `AutoEllipsis`
 * —— 它把全文留在 DOM 里、只在真溢出时挂 tooltip，所以"不换行"不等于"看不见"。
 * 宁可让表格横向滚动（外层 `TABLE_WRAP_SX` 已经 `overflowX: auto`），
 * 也不要一个单元格折成三行、把整行撑到 6 行高。
 */
const CELL_NOWRAP_SX = { whiteSpace: "nowrap" } as const;

// ================================================================ ① 当前出口

function ExitRow({ label, children }: { label: string; children: ReactNode }) {
  return (
    <Box sx={{ display: "flex", alignItems: "center", gap: 2, minWidth: 0 }}>
      <Typography variant="caption" sx={{ color: "text.secondary", minWidth: 96, flex: "0 0 auto" }}>
        {label}
      </Typography>
      <Box sx={{ display: "inline-flex", alignItems: "center", gap: 2, minWidth: 0 }}>{children}</Box>
    </Box>
  );
}

function CurrentExit({
  status,
  lanes,
  kernelPath,
}: {
  status: AppStatus | undefined;
  lanes: readonly LaneRow[];
  /** `net.kernel.path`：`undefined` = 还没读到；`""` = 读到了、用户没配（[19 §2.2]）。 */
  kernelPath: string | undefined;
}) {
  const { t } = useTranslation();
  const gateway = status?.gateway;
  // 只有后端给了 id 才显示出口；能给名字就给名字，给不了解析就原样显示 id（不猜一个线路名）
  const laneId = status?.default_lane_id ?? undefined;
  const laneName = laneId === undefined ? undefined : laneNameOf(laneId, lanes);

  // 「未配内核路径」= **读到了**配置、且 `net.kernel.path` 是空的（[19 §2.2]）。
  // 这时出口就是**直连** —— 不是错误态、也不是"未启用"（网络代理默认开启，默认直连）。
  // `undefined`（还没读到）不算：那是"不知道"，不能冒充"直连"。
  const kernelNotConfigured = kernelPath !== undefined && kernelPath.trim() === "";
  const kernelPathValue = kernelPath === undefined ? undefined : kernelPath.trim();

  return (
    <SectionPaper
      title={t("network.currentExit.title")}
      // 说明只有一句、且本来就是解释（为什么没有值就是「未读取」）：直接构造问号，
      // 让极简守卫看得见它在 <HelpTip … /> 里（见 `SectionPaper` 的 `help` prop 注释）。
      help={
        <HelpTip text={t("network.currentExit.hint")} testId="network-current-exit-help" />
      }
      testId="network-current-exit"
    >
      <Stack spacing={2}>
        {kernelNotConfigured ? (
          // 「出口 · 直连」：用户看得懂的一句话，不是错误提示
          <Typography variant="body1" data-testid="network-direct-label">
            {`${t("status.exit")} · ${t("network.directLabel")}`}
          </Typography>
        ) : null}

        <ExitRow label={t("network.currentExit.gateway")}>
          {gateway === undefined ? (
            <Unreadable />
          ) : (
            <StatusDot
              status={gateway === "running" ? "ok" : "warn"}
              label={gateway === "running" ? t("status.gateway.running") : t("status.gateway.paused")}
            />
          )}
        </ExitRow>

        <ExitRow label={t("network.currentExit.kernel")}>
          {kernelNotConfigured ? (
            // **不点红点**：没有内核路径只说明"走直连"，不是故障
            <Typography variant="body1" data-testid="network-kernel-not-configured" sx={{ color: "text.secondary" }}>
              {t("network.kernel.notConfigured")}
            </Typography>
          ) : (
            <>
              <Box component="span" data-testid="network-kernel-state-dot">
                <KernelStateDot state={status?.kernel.state} />
              </Box>
              {kernelPathValue === undefined || kernelPathValue === "" ? null : (
                <Typography
                  variant="caption"
                  component="span"
                  data-testid="network-kernel-path-value"
                  sx={{ color: "text.secondary", fontFamily: typography.fontMono, overflowWrap: "anywhere" }}
                >
                  {kernelPathValue}
                </Typography>
              )}
            </>
          )}
        </ExitRow>

        <ExitRow label={t("network.currentExit.defaultLane")}>
          {kernelNotConfigured ? (
            <Typography variant="body1">{t("network.directLabel")}</Typography>
          ) : laneId === undefined ? (
            <Unreadable />
          ) : (
            <Typography variant="body1">{laneName ?? laneId}</Typography>
          )}
        </ExitRow>

        <ExitRow label={t("network.currentExit.node")}>
          {status?.current_node === undefined || status.current_node === null ? (
            <Unreadable />
          ) : (
            <Typography variant="body1">{status.current_node}</Typography>
          )}
        </ExitRow>
      </Stack>
    </SectionPaper>
  );
}

// ================================================================ ② 订阅（读 + 写）

/** 表单里的一行：标签 + 控件。窄屏时换行（`flexWrap`），不做"响应式隐藏"。 */
const FIELD_ROW_SX = { flexWrap: "wrap", alignItems: "flex-start" } as const;

/** 一个输入字段：`testId` 挂在容器上（测试用 `within(...).getByRole("textbox")` 取输入框）。 */
function Field({
  testId,
  label,
  value,
  onChange,
  hint,
  minWidth = 200,
  placeholder,
  disabled = false,
}: {
  testId: string;
  label: string;
  value: string;
  onChange: (value: string) => void;
  hint?: string;
  minWidth?: number;
  /** 编辑时用**遮蔽过的**现值当占位符（当前地址长什么样 + 留空 = 不改）。 */
  placeholder?: string;
  /** 主键字段（例如订阅 id）在编辑时锁死：改它等于换一条实体。 */
  disabled?: boolean;
}) {
  return (
    <Stack spacing={1} data-testid={testId} sx={{ minWidth: 0 }}>
      <TextField
        size="small"
        label={label}
        value={value}
        disabled={disabled}
        placeholder={placeholder}
        onChange={(event) => onChange(event.target.value)}
        // 数字字段用 `inputMode` 而不是 `type="number"`：留一个真正可编辑的文本框，
        // 校验（是非负整数吗）由我们自己说清楚，而不是靠浏览器静默丢掉非法输入。
        slotProps={{ htmlInput: { sx: { minWidth } } }}
      />
      {hint === undefined ? null : <Caption>{hint}</Caption>}
    </Stack>
  );
}

/** 写操作的错误区：一律 `errorMessage`（带 TA 码），不吞错、不改写成一句"保存失败"。 */
function WriteError({ testId, error }: { testId: string; error: unknown }) {
  if (error === undefined || error === null) return null;
  return (
    <Typography
      variant="body1"
      data-testid={testId}
      sx={{ color: "error.main", overflowWrap: "anywhere" }}
    >
      {errorMessage(error)}
    </Typography>
  );
}

/** 表单里的本地校验消息（必填 / 数字格式）：**说清缺什么**，而不是把空值发给后端。 */
function FormNotice({ testId, message }: { testId: string; message: string | undefined }) {
  if (message === undefined) return null;
  return (
    <Typography
      variant="body1"
      data-testid={testId}
      sx={{ color: "error.main", overflowWrap: "anywhere" }}
    >
      {message}
    </Typography>
  );
}

const EMPTY_SUBSCRIPTION_DRAFT: SubscriptionDraft = {
  id: "",
  name: "",
  url: "",
  format: "auto",
  // 表单里显示的值就是发出去的值；后端默认值也是这两个（21_600s / 3 个版本）
  intervalS: "21600",
  keepVersions: "3",
  include: "",
  exclude: "",
  enabled: true,
  // [docs/46 §E] 留空 = 不发这个字段（后端默认探测目标）；无公网环境里可填本机地址。
  healthUrl: "",
};

/**
 * [docs/57] **新建 / 编辑订阅的对话框**（用户第 324 轮：「这个界面不要常驻，应该点击添加才会有，
 * 或者说点击编辑」）。
 *
 * 为什么从"常驻表单"改成按需出现：那张表单有 11 个字段，长期占着列表下方，把只读的列表挤成
 * 半屏，而它绝大多数时间用不到。现在由两个入口打开：区块里的「添加订阅」、以及每行的「编辑」。
 *
 * ## 编辑与新建有两处**必须**不同（否则会静默写坏）
 *
 * 1. **地址框刻意留空** —— 列表里的地址是遮蔽过的（`maskSubscriptionUrl` 是唯一出口）。
 *    把遮蔽值回填进输入框，用户一保存就会把 `token=***` 当成真地址写回配置：
 *    保存那一刻不报错、还显示"已保存"，下次刷新才全挂。所以编辑时**留空 = 不改地址**，
 *    由后端 `upsert_subscription` 保证（[docs/57]，同 `credential_set_secret` 的"留空 = 不改密钥"）。
 * 2. **载荷以原始记录为底**（`subscriptionEditPayload`）—— `subscription_upsert` 是**整条覆盖写**。
 *    不带底稿就会把表单没有的字段重置成默认值，最要命的是 `disabled_nodes`
 *   （[docs/54] 逐节点开关）：它被清空 = 用户关掉的节点**全部被重新打开**，而界面上不会报任何错。
 *
 * 成功区显示的**是 `subscription_upsert` 的返回值**（经 `readSubscription` 归一并遮蔽地址），
 * 不是前端拼的一行"添加成功" —— 后者在"后端其实没存进去"时也会显示成功。
 */
function SubscriptionWriteDialog({
  row,
  onClose,
}: {
  /** `undefined` = 新建；否则是正在编辑的那一行。 */
  row?: SubscriptionRow;
  onClose: () => void;
}) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const editing = row !== undefined;
  // 草稿只在挂载时算一次。父组件是**按需挂载**本组件（关闭即卸载）→ 换一行编辑时状态天然是新的，
  // 不会把上一行的输入带过来。
  const [draft, setDraft] = useState<SubscriptionDraft>(() =>
    row === undefined ? EMPTY_SUBSCRIPTION_DRAFT : subscriptionDraftFromRow(row),
  );
  const [formError, setFormError] = useState<string | undefined>(undefined);
  const upsert = useCommandMutation("subscription_upsert");
  const preview = useCommandMutation("subscription_preview");

  const set = <K extends keyof SubscriptionDraft>(key: K, value: SubscriptionDraft[K]) =>
    setDraft((previous) => ({ ...previous, [key]: value }));

  const saved = upsert.data === undefined ? undefined : readSubscription(upsert.data);
  const savedFormat = saved === undefined ? undefined : subscriptionFormatLabel(saved.format);

  const onPreview = () => {
    setFormError(undefined);
    upsert.reset();
    preview.reset();
    if (draft.url.trim() === "") {
      // 编辑时地址本来就是空的（不回显），预览必须用**新填的**地址 —— 所以这里如实说清，
      // 而不是拿一个可能是遮蔽值的旧地址去"预览"。
      setFormError(t("network.subscriptions.write.required"));
      return;
    }
    preview.mutate({ url: draft.url.trim(), format: draft.format });
  };

  const onSubmit = () => {
    setFormError(undefined);
    upsert.reset();
    preview.reset();
    // [docs/57] 编辑时**不要求**地址（留空 = 不改）；新建时必填。
    if (!subscriptionDraftComplete(draft, { urlRequired: !editing })) {
      setFormError(t("network.subscriptions.write.required"));
      return;
    }
    if (
      parseOptionalCount(draft.intervalS) === null ||
      parseOptionalCount(draft.keepVersions) === null
    ) {
      setFormError(t("network.subscriptions.write.badNumber"));
      return;
    }
    upsert.mutate(
      {
        subscription:
          row === undefined
            ? subscriptionPayload(draft)
            : // [docs/57] 编辑：以原始记录为底，只覆盖表单管的键
              subscriptionEditPayload(draft, row.base),
      },
      {
        // 后端真值回来之后重新拉列表：**不用本地状态拼一行"添加成功"**
        onSuccess: () => {
          void queryClient.invalidateQueries({ queryKey: qk.subscriptions });
        },
      },
    );
  };

  const previewData = preview.data;

  return (
    <Dialog
      open
      onClose={onClose}
      fullWidth
      maxWidth="md"
      data-testid="network-subscriptions-write"
      slotProps={{ paper: { sx: { maxHeight: "90vh" } } }}
    >
      <DialogTitle>
        {editing
          ? t("network.subscriptions.write.editTitle")
          : t("network.subscriptions.write.title")}
        <HelpTip
          text={t("network.subscriptions.write.hint")}
          testId="network-subscriptions-write-help"
        />
      </DialogTitle>

      <DialogContent>
        <Stack spacing={3}>
          <Stack spacing={2} sx={FIELD_ROW_SX} direction="row">
            <Field
              testId="network-subscriptions-add-id"
              label={t("network.subscriptions.write.id")}
              value={draft.id}
              // 编辑时 id 是主键，**改它就等于换一条订阅**（upsert 按 id 命中）。
              // 所以锁死：想换 id 就新建一条，别让"编辑"静默改名。
              disabled={editing}
              onChange={(value) => set("id", value)}
              hint={t("network.subscriptions.write.idHint")}
            />
        <Field
          testId="network-subscriptions-add-name"
          label={t("network.subscriptions.write.name")}
          value={draft.name}
          onChange={(value) => set("name", value)}
        />
      </Stack>

      <Field
        testId="network-subscriptions-add-url"
        label={t("network.subscriptions.write.url")}
        value={draft.url}
        onChange={(value) => set("url", value)}
        /*
         * [docs/57] 编辑时地址**故意不回显**（列表里那份是遮蔽过的 `token=***`，
         * 回填再保存就会把 `***` 写成真地址）。所以这里换一句提示 + 用遮蔽值当占位符：
         * 用户能看见"当前是这个（已遮蔽）"，但不填就等于不改。
         */
        placeholder={editing ? row?.url : undefined}
        hint={
          editing ? t("network.subscriptions.write.urlKeepHint") : t("network.subscriptions.write.urlHint")
        }
        minWidth={360}
      />

      <Field
        testId="network-subscriptions-add-health-url"
        label={t("network.subscriptions.write.healthUrl")}
        value={draft.healthUrl}
        onChange={(value) => set("healthUrl", value)}
        hint={t("network.subscriptions.write.healthUrlHint")}
        minWidth={360}
      />

      <Stack spacing={2} sx={FIELD_ROW_SX} direction="row">
        <Stack spacing={1} data-testid="network-subscriptions-add-format" sx={{ minWidth: 200 }}>
          <TextField
            select
            size="small"
            label={t("network.subscriptions.write.format")}
            value={draft.format}
            onChange={(event) => set("format", event.target.value as SubscriptionDraft["format"])}
          >
            {SUBSCRIPTION_FORMATS.map((format) => {
              const labelKey = subscriptionFormatLabel(format);
              return (
                <MenuItem key={format} value={format}>
                  {labelKey === undefined ? format : t(labelKey)}
                </MenuItem>
              );
            })}
          </TextField>
        </Stack>
        <Field
          testId="network-subscriptions-add-interval"
          label={t("network.subscriptions.write.interval")}
          value={draft.intervalS}
          onChange={(value) => set("intervalS", value)}
        />
        <Field
          testId="network-subscriptions-add-keep-versions"
          label={t("network.subscriptions.write.keepVersions")}
          value={draft.keepVersions}
          onChange={(value) => set("keepVersions", value)}
        />
      </Stack>

      <Stack spacing={2} sx={FIELD_ROW_SX} direction="row">
        <Field
          testId="network-subscriptions-add-include"
          label={t("network.subscriptions.write.include")}
          value={draft.include}
          onChange={(value) => set("include", value)}
        />
        <Field
          testId="network-subscriptions-add-exclude"
          label={t("network.subscriptions.write.exclude")}
          value={draft.exclude}
          onChange={(value) => set("exclude", value)}
        />
      </Stack>

      <FormControlLabel
        data-testid="network-subscriptions-add-enabled"
        control={
          <Checkbox
            size="small"
            checked={draft.enabled}
            onChange={(event) => set("enabled", event.target.checked)}
          />
        }
        label={t("network.subscriptions.write.enabled")}
      />

      <FormNotice testId="network-subscriptions-write-error" message={formError} />
      <WriteError testId="network-subscriptions-write-error" error={upsert.error ?? preview.error} />
      {saved === undefined ? null : (
        <Typography variant="body1" data-testid="network-subscriptions-write-result" sx={{ overflowWrap: "anywhere" }}>
          {`${t("network.subscriptions.write.saved")}：${saved.name ?? saved.id} · ${saved.id}`}
          {savedFormat === undefined ? "" : ` · ${saved.format === undefined ? "" : t(savedFormat)}`}
          {saved.intervalS === undefined ? "" : ` · ${formatDuration(saved.intervalS)}`}
          {saved.keepVersions === undefined
            ? ""
            : ` · ${t("network.subscriptions.col.keepVersions")} ${saved.keepVersions}`}
          {` · ${saved.enabled ? t("common.enabled") : t("common.disabled")}`}
          {saved.url === "" ? "" : ` · ${saved.url}`}
        </Typography>
      )}

      {previewData === undefined ? null : (
        <Typography variant="body1" data-testid="network-subscriptions-preview-result" sx={{ overflowWrap: "anywhere" }}>
          {`${t("network.subscriptions.write.previewTitle")}：`}
          {t("network.subscriptions.write.previewCounts", {
            groups: previewData.groups.length,
            nodes: previewData.nodes.length,
          })}
          {previewData.sample.length === 0
            ? ""
            : ` · ${t("network.subscriptions.write.previewSample", {
                sample: previewData.sample.map((node) => node.name).join(" · "),
              })}`}
        </Typography>
      )}
        </Stack>
      </DialogContent>

      {/* [docs/28 §1.7] **主操作放在对话框的固定按钮区**（不参与内容滚动）：
          长表单滚到底也能看见「保存」；「关闭」永远在，不逼用户去点遮罩。 */}
      <DialogActions>
        <Button data-testid="network-subscriptions-write-close" onClick={onClose}>
          {t("common.close")}
        </Button>
        <Button variant="outlined" data-testid="network-subscriptions-preview" onClick={onPreview}>
          {t("network.subscriptions.write.preview")}
        </Button>
        <Button variant="contained" data-testid="network-subscriptions-add-submit" onClick={onSubmit}>
          {t("network.subscriptions.write.submit")}
        </Button>
      </DialogActions>
    </Dialog>
  );
}

/**
 * 一行上的**流量**（[18 §1.3]，等价于 Clash 面板那一行）：更新时间 + 已用/总量 + 百分比 + 到期。
 *
 * **三条各自独立，缺哪样就不显示那一样**（不显示 0、不显示占位符）：
 * 数字全部来自后端（`subscription_traffic` 的持久化观测值），前端只做格式化 ——
 * 不自己算 `total`、不把缺失当 0（`used = upload + download` 要两者都有才算）。
 *
 * 三态不混：`traffic === undefined`（还没读到）→「未读取」；`null`（从没成功刷新过）→「尚未获取」。
 *
 * **第 326 轮起它画在订阅详情页**（列表上那 14 列砍到 6 列，流量属于"点进去再看"的那一类）：
 * 所以这里导出给 `./subscription` 用，页面自己在数据层把 `subscription_traffic` 合流进来。
 */
export function TrafficCell({ traffic }: { traffic: TrafficRow | null | undefined }) {
  const { t } = useTranslation();
  if (traffic === undefined) return <Unreadable />;
  const summary = summarizeTraffic(traffic);
  if (summary.kind === "none") {
    return (
      <Typography variant="body1" component="span" sx={{ color: "text.secondary" }}>
        {t("network.sub.traffic.none")}
      </Typography>
    );
  }
  const parts: string[] = [];
  if (summary.fetchedAtMs !== undefined) {
    // 本地时区 `YYYY-MM-DD HH:mm`（Clash 面板同款）
    parts.push(t("network.sub.traffic.updated", { at: formatLocalTime(summary.fetchedAtMs, "YYYY-MM-DD HH:mm") }));
  }
  if (summary.usedBytes !== undefined) {
    parts.push(
      summary.totalBytes === undefined
        ? t("network.sub.traffic.usedOnly", { used: formatBytes(summary.usedBytes) })
        : t("network.sub.traffic.used", {
            used: formatBytes(summary.usedBytes),
            total: formatBytes(summary.totalBytes),
          }),
    );
  }
  if (summary.usedPct !== undefined) {
    parts.push(t("network.sub.traffic.percent", { pct: summary.usedPct }));
  }
  if (summary.expireAtS !== undefined) {
    // 契约是 **Unix 秒**
    parts.push(t("network.sub.traffic.expire", { date: formatLocalTime(summary.expireAtS * 1000, "YYYY-MM-DD") }));
  }
  if (parts.length === 0) return <Unreadable />;
  return (
    <Typography variant="body1" component="span">
      {parts.join(" · ")}
    </Typography>
  );
}

// ================================================================ 删除（[docs/21 §1]）

/**
 * 一次删除的目标：**单条与批量共用同一个形状**（[docs/21 §1.2] 要求同一套语义）。
 *
 * 订阅与线路这两条命令（`subscription_delete` / `lane_delete`）**都没有 `force` 语义**
 * （[docs/21 §1.1 第 3 条]）→ 被引用时只显示后端原文，**一个"强行删除"按钮都不放**（那会是假按钮）。
 */
type DeleteTarget = {
  /** 要删的实体 id（顺序 = 勾选顺序）。 */
  readonly ids: readonly string[];
  /** 与 `ids` 一一对应的显示名（名称 + id）：确认框与结果行都用它写清"删的是哪一个"。 */
  readonly labels: readonly string[];
  /** 批量：**一次确认** → 逐条调用（[docs/21 §1.2]）。 */
  readonly batch: boolean;
  /** 「会连带删除 / 影响」清单：只放前端**算得出来**的事实；算不出来就是空数组（不编）。 */
  readonly consequences: readonly string[];
};

/** 执行完的**事实**（文案由各页自己用 `t(...)` 拼，这里不带任何文案）。 */
type DeleteOutcome = {
  deletedIds: string[];
  failures: { id: string; message: string }[];
};

/**
 * 逐条执行删除：**一条失败不中止其余条**、失败原样记下后端原文（[docs/21 §1.2]）。
 * 与 `pages/models/index.tsx` 的同名函数是同一套语义（这些命令都没有 force 参数）。
 */
async function runDelete(
  target: DeleteTarget,
  remove: (id: string) => Promise<unknown>,
): Promise<DeleteOutcome> {
  const deletedIds: string[] = [];
  const failures: { id: string; message: string }[] = [];
  for (const id of target.ids) {
    try {
      await remove(id);
      deletedIds.push(id);
    } catch (err) {
      failures.push({ id, message: errorMessage(err) });
    }
  }
  return { deletedIds, failures };
}

/** 「名称 + id」：名称缺失（后端就是没给）时就只写 id，**不编一个名字**。 */
function entityLabel(name: string | undefined, id: string): string {
  return name === undefined || name === "" ? id : `${name}（${id}）`;
}

/** 结果区的一行：`target` 里记着显示名，所以成功项写的是**名称**而不是 id（[docs/21 §1.1 第 4 条]）。 */
function labelOf(target: DeleteTarget, id: string): string {
  const index = target.ids.indexOf(id);
  return index < 0 ? id : (target.labels[index] ?? id);
}

function SubscriptionsSection({
  rows,
  laneRows,
  isLoading,
  error,
  onRetry,
  subscriptionHref,
}: {
  rows: readonly SubscriptionRow[] | undefined;
  /** `lane_list` 的归一化结果：**只**用来算"这份订阅被几条线路引用"（算不出来就留空）。 */
  laneRows: readonly LaneRow[] | undefined;
  isLoading: boolean;
  error: unknown;
  onRetry: () => void;
  /**
   * 「点订阅名进这条订阅的详情页」的链接地址（[docs/54]）。
   *
   * 为什么是**传进来的**而不是在这里直接写 `<Link to=…>`：`NetworkBody` 是纯 props 展示层，
   * 它的单测**不包 Router**（见 `showProviderReturn` 的同一条理由与注释）。
   * 在展示层里用 `Link` 会让所有渲染它的测试炸在
   * `Cannot destructure property 'basename' of useContext(...)` 上 —— 那不是产品问题，
   * 是"展示层偷偷依赖了路由上下文"。所以由数据层拼好 href 传进来；
   * 传 `undefined`（例如在不知道路由的地方渲染）就退化成纯文本，**不报错、不白屏**。
   */
  subscriptionHref?: (id: string) => string;
}) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  // 行级写操作：刷新（强制重下）与删除。都**不碰系统网络**，只改 TokenAgency 自己的配置 /
  // 订阅版本存储；删完 / 刷完重新拉列表，显示的是后端下一次给的事实。
  const refreshOne = useCommandMutation("subscription_refresh");
  const removeOne = useCommandMutation("subscription_delete");

  // ---- 删除状态（[docs/21 §1]）：单条与批量**共用**同一个确认组件、同一套失败展示
  /** 多选（按 id 存；行 `network-subscriptions-select` / 表头 `-select-all` / 计数 `-selected-count`）。 */
  const [selectedIds, setSelectedIds] = useState<readonly string[]>([]);
  /** 确认框里那一次操作（`undefined` = 框没开）。**确认之前一条命令都不发**。 */
  const [deleteTarget, setDeleteTarget] = useState<DeleteTarget | undefined>(undefined);
  /** 后端原文（被引用时的错误）：有它才显示；订阅没有 force 语义 → 级联按钮一个都不放。 */
  const [deleteError, setDeleteError] = useState<string | undefined>(undefined);
  /** 结果区：**只描述最近一次**删除操作（上一次的结论不该混进这一次）。 */
  const [deleteResult, setDeleteResult] = useState<readonly string[]>([]);
  /** 批量进行中（逐条 `mutateAsync` 之间 `isPending` 会回落，所以单独记一笔）。 */
  const [batchRunning, setBatchRunning] = useState(false);

  /**
   * [docs/57] 写表单**不再常驻**（用户第 324 轮：「这个界面不要常驻，应该点击添加才会有，
   * 或者说点击编辑」）：`undefined` = 关着；`{}` = 新建；`{ row }` = 编辑那一行。
   *
   * 用**按需挂载**（关着时 `null`）而不是 `open` 开关：草稿状态随卸载一起消失，
   * 于是"换一行编辑"永远不会把上一行的输入带过来 —— 那是个会静默改错实体的坑。
   */
  const [writeTarget, setWriteTarget] = useState<{ row?: SubscriptionRow } | undefined>(undefined);

  const busy = refreshOne.isPending || removeOne.isPending || batchRunning;

  /**
   * **有没有"多选"这个场景**：≥2 条订阅才有（规则 3：没有多选场景就不出现勾选框）。
   *
   * 只有一条订阅时，那个选中计数与「删除选中」按钮、以及行里那个勾选框，都是纯噪音
   * —— 一条订阅的"批量删除"就是那一行自己的「删除」。真有多条时（`proxy_subscriptions`
   * 本来就是一份清单，用户可以加任意多份）这些控件一个不少地出现。
   */
  const multiSelectable = rows !== undefined && rows.length > 1;

  /**
   * 删除成功后要失效的**全部**受影响的查询（[docs/21 §1.1 第 4 条]）：
   * `lane_list` 里每条线路都引用着某个订阅 id，删掉订阅之后线路页签 / 线路下拉的选择项都会变
   * —— 只失效订阅列表会让它们停在旧值上（那正是"看起来删了、其实界面还是旧的"）。
   */
  const invalidateAfterDelete = async (): Promise<void> => {
    // [docs/55] `qk.lanePickOptions` 已随 `lane_pick_options` 命令删除 —— 去掉它。
    for (const key of [qk.subscriptions, qk.lanes]) {
      await queryClient.invalidateQueries({ queryKey: key });
    }
  };

  /**
   * 「会连带删除 / 影响」：引用了这份订阅的线路条数（来自 `lane_list`，本页本来就在读）。
   * `lane_list` 还没读到 / 读失败 → **空数组**：算不出来就不写，绝不编一个数（[docs/21 §1.1 第 3 条]）。
   */
  const consequencesOf = (row: SubscriptionRow): string[] => {
    if (laneRows === undefined) return [];
    const count = laneRows.filter((lane) => lane.subscriptionId === row.id).length;
    return count === 0 ? [] : [t("delete.consequence.subscriptionLanes", { value: formatInt(count) })];
  };

  const openDelete = (target: DeleteTarget): void => {
    setDeleteTarget(target);
    setDeleteError(undefined);
    // 新一次操作开始 → 清掉上一次的汇报（结果区永远只讲刚刚发生的事）
    setDeleteResult([]);
  };

  const confirmDelete = async (): Promise<void> => {
    const target = deleteTarget;
    if (target === undefined) return;
    if (target.batch) setBatchRunning(true);
    const outcome = await runDelete(target, (id) => removeOne.mutateAsync({ id }));
    setBatchRunning(false);
    const singleFailed = !target.batch && outcome.failures.length > 0;
    setDeleteResult(
      target.batch
        ? [
            t("delete.result.summary", {
              ok: formatInt(outcome.deletedIds.length),
              failed: formatInt(outcome.failures.length),
            }),
            ...outcome.deletedIds.map((id) =>
              t("delete.result.ok", { name: labelOf(target, id) }),
            ),
            ...outcome.failures.map((failure) =>
              t("delete.result.failed", { message: `${failure.id} · ${failure.message}` }),
            ),
          ]
        : // 单条失败时结果区留空：后端原文就在确认框里（`deleteError`），不在这里再抄一遍
          singleFailed
          ? []
          : outcome.deletedIds.map((id) => t("delete.result.ok", { name: labelOf(target, id) })),
    );
    setDeleteError(singleFailed ? outcome.failures[0]?.message : undefined);
    // 只有真的删掉的才从选中里摘掉；失败的留着（用户可以直接重试）
    setSelectedIds((prev) => prev.filter((id) => !outcome.deletedIds.includes(id)));
    // 单条失败时**留着框**：后端原文就在那里；其余情况收框
    if (target.batch || !singleFailed) setDeleteTarget(undefined);
    await invalidateAfterDelete();
  };

  const toggleSelect = (id: string): void => {
    setSelectedIds((prev) =>
      prev.includes(id) ? prev.filter((selectedId) => selectedId !== id) : [...prev, id],
    );
  };

  /** 表头全选：对**整个订阅列表**生效（这一块没有筛选器，列表本身就是可见集合）。 */
  const toggleSelectAll = (checked: boolean): void => {
    setSelectedIds(checked ? (rows ?? []).map((row) => row.id) : []);
  };

  const onRefresh = (id: string) => {
    refreshOne.reset();
    removeOne.reset();
    refreshOne.mutate(
      { id, force: true },
      {
        onSuccess: () => {
          void queryClient.invalidateQueries({ queryKey: qk.subscriptions });
        },
      },
    );
  };

  /** 单条删除：**先开确认框**（[docs/21 §1.1 第 2 条]），确认之后才调命令。 */
  const onDelete = (row: SubscriptionRow) => {
    refreshOne.reset();
    removeOne.reset();
    openDelete({
      ids: [row.id],
      labels: [entityLabel(row.name, row.id)],
      batch: false,
      consequences: consequencesOf(row),
    });
  };

  /** 「删除选中」：一次确认 → 逐条调用（[docs/21 §1.2]）。 */
  const onDeleteSelected = () => {
    refreshOne.reset();
    removeOne.reset();
    const chosen = (rows ?? []).filter((row) => selectedIds.includes(row.id));
    if (chosen.length === 0) return;
    openDelete({
      ids: chosen.map((row) => row.id),
      labels: chosen.map((row) => entityLabel(row.name, row.id)),
      batch: true,
      // 批量的连带清单 = 每一条各自的清单（算不出来的就是空），逐条标明是谁的
      consequences: chosen.flatMap((row) =>
        consequencesOf(row).map((line) => `${entityLabel(row.name, row.id)}：${line}`),
      ),
    });
  };

  const refreshResult = refreshOne.data;

  return (
    <SectionPaper
      title={t("network.subscriptions.title")}
      // 一个区块**只有这一个**问号（[docs/22 §3]）：说明 + 三句解释（地址遮蔽、健康检查的语义、
      // 刷新为什么强制重下）合成同一条提示。原来那三句挂在标题**下面单独一行**，
      // 就变成用户真机截图里那个"悬在工具栏上方、没有标签的孤儿问号"。
      description={mergeHelpText(
        t("network.subscriptions.description"),
        t("network.subscriptions.urlHint"),
        t("network.subscriptions.healthHint"),
        t("network.subscriptions.row.refreshHint"),
      )}
      testId="network-subscriptions"
    >
      {/* 「节点数 / 组数从哪来」那一句（`network.subscriptions.countHint`）已作废：
          它写着"本页尚未接入 subscription_nodes / subscription_groups"，而这两条命令
          这一页真的在调（见 `LaneWriteForm`）；/network/nodes 上更是直接列出了节点清单（[19 §2.3]）。 */}
      {/* [docs/57] **写表单的入口在这里**（它不再常驻）。这一排**永远渲染**：
          订阅一条都没有时，用户也必须有个地方能点"添加" —— 否则空列表就是个死路。 */}
      {/* **多选只在真的存在多选场景时才出现**（规则 3）：只有一条订阅时，
          "已选 0 条 / 删除选中"与行内那个勾选框都是纯噪音（一条订阅的批量删除 = 单条删除），
          所以它们要 ≥2 条才出现。真有多条时功能一个字不少。 */}
      <Stack direction="row" spacing={2} alignItems="center" sx={{ flexWrap: "wrap" }}>
        <Button
          variant="contained"
          data-testid="network-subscriptions-add-open"
          onClick={() => setWriteTarget({})}
        >
          {t("network.subscriptions.write.open")}
        </Button>
        {multiSelectable ? (
          <>
            <Typography variant="body1" data-testid="network-subscriptions-selected-count">
              {t("delete.selectedCount", { count: selectedIds.length })}
            </Typography>
            <Button
              variant="outlined"
              color="error"
              data-testid="network-subscriptions-delete-selected"
              disabled={selectedIds.length === 0 || busy}
              onClick={onDeleteSelected}
            >
              {t("common.deleteSelected")}
            </Button>
          </>
        ) : null}
      </Stack>

      {error !== undefined && error !== null ? (
        <Stack direction="row" spacing={2} alignItems="center" sx={{ flexWrap: "wrap" }}>
          <Typography variant="body1" data-testid="network-subscriptions-error" sx={{ color: "error.main" }}>
            {errorMessage(error)}
          </Typography>
          <Button variant="outlined" onClick={onRetry}>
            {t("common.retry")}
          </Button>
        </Stack>
      ) : rows === undefined ? (
        <Typography variant="body1" sx={{ color: "text.secondary" }}>
          {isLoading ? t("common.loading") : DASH}
        </Typography>
      ) : rows.length === 0 ? (
        <Typography variant="body1" data-testid="network-subscriptions-empty">
          {t("network.subscriptions.empty")}
        </Typography>
      ) : (
        <Box sx={TABLE_WRAP_SX}>
          {/*
            * **第 326 轮：14 列砍到 6 列**（规则 4）。
            *
            * 原来那张表把"配置里声明的每一样"都摊成一列，于是表头被挤成逐字竖排
            * （`自动识别` → `自 动 识 别`）、`最近更新时间：2026-10-07 06:31:51` 把整行撑到 6 行高。
            * 现在留在列表上的只有"扫一眼要看的"六样：名称（也是进详情页的入口）、地址、启用、
            * 节点数、最近更新、操作（行级动作）。
            *
            * 砍掉的那些**没有消失**：格式 / 更新周期 / 保留版本 / 过滤 / 健康检查 / 流量 /
            * 组数 / 解析器 / 错误原文全部在**详情页**（`/network/subscriptions/:id`）逐项列出 ——
            * 那是一份订阅的"详情抽屉"，点名称就进得去。错误在列表上只留一个红色的「解析失败」结论。
            */}
          <Table size="small" data-testid="network-subscriptions-table">
            <TableHead>
              <TableRow>
                {/* 表头全选（`<page>-select-all`）：**只在真有多选场景时才出现**（见 `multiSelectable`） */}
                {multiSelectable ? (
                  <TableCell padding="checkbox">
                    <Box data-testid="network-subscriptions-select-all">
                      <Checkbox
                        size="small"
                        checked={rows.every((row) => selectedIds.includes(row.id))}
                        indeterminate={
                          rows.some((row) => selectedIds.includes(row.id)) &&
                          !rows.every((row) => selectedIds.includes(row.id))
                        }
                        onChange={(event) => toggleSelectAll(event.target.checked)}
                      />
                    </Box>
                  </TableCell>
                ) : null}
                <TableCell sx={CELL_NOWRAP_SX}>{t("network.subscriptions.col.name")}</TableCell>
                <TableCell sx={CELL_NOWRAP_SX}>{t("network.subscriptions.col.url")}</TableCell>
                <TableCell sx={CELL_NOWRAP_SX}>{t("network.subscriptions.col.enabled")}</TableCell>
                <TableCell align="right" sx={CELL_NOWRAP_SX}>
                  {t("network.subscriptions.col.nodes")}
                </TableCell>
                <TableCell sx={CELL_NOWRAP_SX}>{t("network.subscriptions.col.lastUpdate")}</TableCell>
                <TableCell align="right" sx={CELL_NOWRAP_SX}>
                  {t("network.subscriptions.col.actions")}
                </TableCell>
              </TableRow>
            </TableHead>
            <TableBody>
              {rows.map((row) => (
                <TableRow key={row.id} hover data-testid="network-subscription-row">
                  {multiSelectable ? (
                    <TableCell padding="checkbox">
                      <Box data-testid="network-subscriptions-select">
                        <Checkbox
                          size="small"
                          checked={selectedIds.includes(row.id)}
                          onChange={() => toggleSelect(row.id)}
                        />
                      </Box>
                    </TableCell>
                  ) : null}
                  <TableCell sx={CELL_NOWRAP_SX}>
                    {/* [docs/54] 订阅名是**进详情页的入口**（用户第 321 轮："再去点击一个订阅，
                        里面就可以看到这个订阅的所有节点"）。`subscriptionHref` 由数据层传进来
                        （见它的 prop 注释：展示层不碰路由上下文）。
                        `AutoEllipsis` 只吃**字符串**子节点，所以它管的是那串用户数据本身。 */}
                    {subscriptionHref === undefined ? (
                      <AutoEllipsis testId="network-subscription-name-ellipsis" sx={{ maxWidth: 200 }}>
                        {row.name === undefined ? DASH : row.name}
                      </AutoEllipsis>
                    ) : (
                      <Link to={subscriptionHref(row.id)} data-testid="network-subscription-open">
                        <AutoEllipsis
                          testId="network-subscription-name-ellipsis"
                          sx={{ maxWidth: 200 }}
                        >
                          {row.name === undefined ? DASH : row.name}
                        </AutoEllipsis>
                      </Link>
                    )}
                  </TableCell>
                  <TableCell sx={{ ...CELL_NOWRAP_SX, fontFamily: typography.fontMono }}>
                    {/* [docs/26 §2] 订阅地址是最长的值之一（域名 + 路径 + 查询串）。
                        恢复路径：本行的订阅名就是详情页入口，完整信息在那一页；此外悬停的
                        tooltip 也能看全（`AutoEllipsis` 只在真溢出时挂提示）。 */}
                    <AutoEllipsis testId="network-subscription-url-ellipsis" sx={{ maxWidth: 280 }}>
                      {row.url === "" ? DASH : row.url}
                    </AutoEllipsis>
                  </TableCell>
                  <TableCell sx={CELL_NOWRAP_SX}>
                    {row.enabled ? t("common.enabled") : t("common.disabled")}
                  </TableCell>
                  <TableCell
                    align="right"
                    sx={{ ...CELL_NOWRAP_SX, fontVariantNumeric: "tabular-nums" }}
                  >
                    {row.nodeCount === undefined ? <Unreadable /> : row.nodeCount}
                  </TableCell>
                  <TableCell sx={{ ...CELL_NOWRAP_SX, fontVariantNumeric: "tabular-nums" }}>
                    {row.error === undefined ? (
                      row.lastUpdateMs === undefined ? (
                        <Unreadable />
                      ) : (
                        // 短格式（`2 分钟前`）：完整时间戳（`2026-10-07 06:31:51`）会把整行撑成 6 行高。
                        // 精确时刻在详情页里逐项列出，这里只需要"新不新"。
                        formatAgo(row.lastUpdateMs)
                      )
                    ) : (
                      // 解析失败：列表上只给结论（红色圆点 + 四个字），**后端原文在详情页里逐字显示**
                      // （规则 2：原文进详情，不摊在列表里把行撑高；也不是"藏起来"—— 点名字就看得见）。
                      <StatusDot status="error" label={t("network.subscriptions.parseFailed")} />
                    )}
                  </TableCell>
                  <TableCell align="right" sx={CELL_NOWRAP_SX}>
                    <Stack direction="row" spacing={2} justifyContent="flex-end">
                      <Button
                        size="small"
                        variant="outlined"
                        data-testid="network-subscriptions-edit"
                        disabled={busy}
                        onClick={() => setWriteTarget({ row })}
                      >
                        {t("network.subscriptions.row.edit")}
                      </Button>
                      <Button
                        size="small"
                        variant="outlined"
                        data-testid="network-subscriptions-refresh"
                        disabled={busy}
                        onClick={() => onRefresh(row.id)}
                      >
                        {t("network.subscriptions.row.refresh")}
                      </Button>
                      <Button
                        size="small"
                        variant="outlined"
                        data-testid="network-subscriptions-delete"
                        disabled={busy}
                        onClick={() => onDelete(row)}
                      >
                        {t("network.subscriptions.row.delete")}
                      </Button>
                    </Stack>
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </Box>
      )}

      {/* 行级写操作的结果：显示后端返回的事实，不是前端拼的一句话 */}
      {refreshResult === undefined ? null : (
        <Typography variant="body1" data-testid="network-subscriptions-refresh-result" sx={{ overflowWrap: "anywhere" }}>
          {`${t("network.subscriptions.write.refreshTitle")}：${
            refreshResult.ok
              ? t("network.subscriptions.write.refreshOk")
              : t("network.subscriptions.write.refreshFailed")
          }`}
          {` · ${t("network.subscriptions.col.nodes")} ${refreshResult.node_count}`}
          {` · ${t("network.subscriptions.col.groups")} ${refreshResult.group_count}`}
          {refreshResult.error === undefined || refreshResult.error === null
            ? ""
            : ` · ${refreshResult.error}`}
        </Typography>
      )}

      {/* 删除结果（[docs/21 §1.1 第 4 条] 的 `network-subscriptions-delete-result`）：
          成功写"已删除 <名称>"；批量先给成功/失败条数，失败项**逐条**带 id + 后端原文。 */}
      {deleteResult.length === 0 ? null : (
        <Stack spacing={1} data-testid="network-subscriptions-delete-result">
          {deleteResult.map((line, index) => (
            <Typography key={`${index}-${line}`} variant="body1" sx={{ overflowWrap: "anywhere" }}>
              {line}
            </Typography>
          ))}
        </Stack>
      )}

      {/* 流量（`subscription_traffic`）与它的读取错误都随流量一起搬到了订阅详情页
          （列表上不再有流量列）—— 那里仍然是"不把失败画成尚未获取"的同一条纪律。 */}

      <WriteError testId="network-subscriptions-row-error" error={refreshOne.error ?? removeOne.error} />

      {/* 删除确认（[docs/21 §1 / §2]）：**先确认再发命令**；被引用时后端原文原样显示。
          `subscription_delete` 没有 force 语义 → `cascadeAvailable={false}`，一个级联按钮都不放。 */}
      <ConfirmDelete
        open={deleteTarget !== undefined}
        testIdPrefix="network-subscriptions"
        title={t("delete.title.subscription")}
        // 确认框里写清删的是哪一个：单条 = 名称 + id；批量 = 选中数 + 逐条标识
        targetLabel={
          deleteTarget === undefined
            ? ""
            : deleteTarget.batch
              ? `${t("delete.selectedCount", { count: deleteTarget.ids.length })} · ${deleteTarget.labels.join("、")}`
              : (deleteTarget.labels[0] ?? "")
        }
        consequences={deleteTarget?.consequences}
        cascadeAvailable={false}
        pending={removeOne.isPending || batchRunning}
        errorText={deleteError}
        onConfirm={() => void confirmDelete()}
        onConfirmCascade={() => {}}
        onCancel={() => {
          setDeleteTarget(undefined);
          setDeleteError(undefined);
        }}
      />

      <Divider />

      {/* [docs/57] 写表单的对话框：**只在点了「添加订阅」或某一行「编辑」时才挂载**。
          第 322 轮以前它是常驻的 `<SubscriptionWriteForm />`，占着列表下方一整屏。 */}
      {writeTarget === undefined ? null : (
        <SubscriptionWriteDialog row={writeTarget.row} onClose={() => setWriteTarget(undefined)} />
      )}
    </SectionPaper>
  );
}

// ================================================================ ③ 线路（只读）
//
// [docs/58]（用户第 325 轮：「这个功能好像没有什么用，可以在界面上删了」）
// **手动新建 / 编辑线路的界面整块删除** —— 线路不再是用户要手填的东西：
// 在服务商页选「自动」/ 某个节点 / 「直连」时，后端会把那条线路**按需建出来**
//（`ensure_auto_lane` / `lane_ensure_for_node` / `lane_ensure_direct`）。
//
// 这一块保留的是**只读 + 行级动作**：看清单、测速、删除。
function LanesSection({
  rows,
  isLoading,
  error,
  onRetry,
  providersHref,
}: {
  rows: readonly LaneRow[] | undefined;
  isLoading: boolean;
  error: unknown;
  onRetry: () => void;
  /**
   * 空态那个「去服务商页」按钮的地址（由数据层给，见 `SubscriptionsSection` 的
   * `subscriptionHref` 注释：展示层不碰路由上下文）。不传就退化成只有一行说明。
   */
  providersHref?: string;
}) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  // 行级写操作：删除（配置实体）、钉住节点（**只改内核运行态**，不改配置）、逐节点测速（[19 §2.4]）。
  const removeOne = useCommandMutation("lane_delete");
  const setNode = useCommandMutation("lane_set_node");
  const testLane = useCommandMutation("lane_test");
  const [nodeDrafts, setNodeDrafts] = useState<Record<string, string>>({});
  const [rowError, setRowError] = useState<string | undefined>(undefined);

  /**
   * 通道矩阵：**只**用来算"当前有几条通道显式走这条线路"（`delete.consequence.laneChannels`）。
   *
   * 为什么是 `channel_matrix` 而不是 `lane_reverse_lookup`：矩阵的每一格就带 `lane_id`，
   * 一次读能覆盖**所有**线路（倒排要逐条问一次）；而且矩阵本来就是"通道挂在哪儿"的唯一真源。
   * 它是**只读**命令（I2 不受影响）。读不到就一个字都不写（[docs/21 §1.1 第 3 条]：算不出来不编）。
   */
  const matrix = useCommandQuery("channel_matrix", undefined, qk.channelMatrix);

  // ---- 删除状态（[docs/21 §1]）：**只有单条删除**（线路表不做批量，见 `⋮` 菜单那段注释）
  /** 确认框里那一次操作（`undefined` = 框没开）。**确认之前一条命令都不发**。 */
  const [deleteTarget, setDeleteTarget] = useState<DeleteTarget | undefined>(undefined);
  /** 后端原文（被引用时的错误）：有它才显示；线路没有 force 语义 → 级联按钮一个都不放。 */
  const [deleteError, setDeleteError] = useState<string | undefined>(undefined);
  /** 结果区：**只描述最近一次**删除操作。 */
  const [deleteResult, setDeleteResult] = useState<readonly string[]>([]);
  const busy = removeOne.isPending || setNode.isPending || testLane.isPending;

  /**
   * 行内动作的 `⋮` 溢出菜单：**整块只用一个菜单实例**（同一时刻只开一个）。
   * `anchorEl` 是被点的那一行的按钮，`menuRow` 是它当前针对的那一行。
   *
   * 为什么不是每行各挂一个 `<Menu>`：一行一份弹出层状态与一份 portal，"哪一行开着"本来就是单值；
   * 一份状态更少出错（也不可能出现两个菜单同时开着）。
   */
  const [menuAnchor, setMenuAnchor] = useState<HTMLElement | null>(null);
  const [menuRow, setMenuRow] = useState<LaneRow | undefined>(undefined);
  const closeMenu = (): void => {
    setMenuAnchor(null);
    setMenuRow(undefined);
  };

  /**
   * 删除成功后要失效的**全部**受影响的查询（[docs/21 §1.1 第 4 条]）：
   * **`qk.channelMatrix`（`["channels","matrix"]`）不在 `qk.lanes` 前缀里** ——
   * 通道矩阵每一格都带 `lane_id`，是"这条线路被哪些通道引用"的真源；不显式失效它，
   * 矩阵就会停在旧值上（"看起来删了、矩阵还指着它"）。列全了也更经得起后人删改。
   *
   * [docs/55] `qk.laneAssignments` / `qk.lanePickOptions` 已随那三条命令一起删除 ——
   * 原来它们挂在 `qk.lanes` 前缀下、由前缀失效顺带覆盖，现在连 key 都不存在了。
   */
  const invalidateAfterDelete = async (): Promise<void> => {
    for (const key of [qk.lanes, qk.channelMatrix]) {
      await queryClient.invalidateQueries({ queryKey: key });
    }
  };

  /**
   * 「会连带删除 / 影响」：通道矩阵里 `lane_id` 等于这条线路的格子数。
   * 矩阵还没读到 / 读失败 → **空数组**（算不出来就不写，绝不编一个数）。
   */
  const consequencesOf = (lane: LaneRow): string[] => {
    if (matrix.data === undefined) return [];
    const count = matrix.data.cells.filter((cell) => cell.lane_id === lane.id).length;
    return count === 0 ? [] : [t("delete.consequence.laneChannels", { value: formatInt(count) })];
  };

  const openDelete = (target: DeleteTarget): void => {
    setDeleteTarget(target);
    setDeleteError(undefined);
    // 新一次操作开始 → 清掉上一次的汇报（结果区永远只讲刚刚发生的事）
    setDeleteResult([]);
  };

  const confirmDelete = async (): Promise<void> => {
    const target = deleteTarget;
    if (target === undefined) return;
    const outcome = await runDelete(target, (id) => removeOne.mutateAsync({ id }));
    const failed = outcome.failures.length > 0;
    // 失败时结果区留空：后端原文就在确认框里（`deleteError`），不在这里再抄一遍
    setDeleteResult(
      failed
        ? []
        : outcome.deletedIds.map((id) => t("delete.result.ok", { name: labelOf(target, id) })),
    );
    setDeleteError(failed ? outcome.failures[0]?.message : undefined);
    // 失败时**留着框**：后端原文就在那里；成功才收框
    if (!failed) setDeleteTarget(undefined);
    await invalidateAfterDelete();
  };

  /** 单条删除：**先开确认框**（[docs/21 §1.1 第 2 条]），确认之后才调命令。 */
  const onDelete = (row: LaneRow) => {
    setRowError(undefined);
    removeOne.reset();
    openDelete({
      ids: [row.id],
      labels: [entityLabel(row.name, row.id)],
      batch: false,
      consequences: consequencesOf(row),
    });
  };

  /** 打开某一行的 `⋮` 菜单：`anchorEl` 指向那一行的按钮，`menuRow` 记住它针对谁。 */
  const openMenu = (event: React.MouseEvent<HTMLElement>, row: LaneRow) => {
    setRowError(undefined);
    setMenuAnchor(event.currentTarget);
    setMenuRow(row);
  };

  const onSetNode = (id: string) => {
    setRowError(undefined);
    setNode.reset();
    const node = (nodeDrafts[id] ?? "").trim();
    if (node === "") {
      setRowError(t("network.lanes.row.setNodeEmpty"));
      return;
    }
    setNode.mutate(
      { id, node },
      {
        onSuccess: () => {
          // 钉住是运行态：列表里的健康样本要重新拉才是新的
          void queryClient.invalidateQueries({ queryKey: qk.lanes });
        },
      },
    );
  };

  const pinned = setNode.isSuccess ? setNode.variables : undefined;
  // 测速结果**原样显示后端的话**：成功就逐节点列延迟，失败就显示后端原文（含 TA 码）。
  const tested = testLane.variables?.id;
  const testFailed = testLane.error !== null && testLane.error !== undefined;

  const onTest = (id: string) => {
    setRowError(undefined);
    testLane.reset();
    testLane.mutate({ id });
  };

  return (
    <SectionPaper
      title={t("network.lanes.title")}
      /*
       * 六句解释与区块说明合成**一个**问号（[docs/22 §3]）：类型词表、通道数的口径、样本从哪来、
       * 钉住改了哪里、直连为什么没有组、测速为什么可能报错。正文一个字符都没丢。
       *
       * 为什么由这里自己构造 `<HelpTip>`，而不是走 `description="…"`（结果一样）：
       * 极简守卫（`uiMinimalism.guard.test.ts` 规则 2）只把**字面量就写在 `<HelpTip … />` 里**
       * 的键算成"按需展开的说明"，多键合并出来的长句会被它保守地误算成版面文案
       * —— 写在这里，守卫看到的就是事实。
       */
      help={
        <HelpTip
          text={mergeHelpText(
            t("network.lanes.description"),
            t("network.lanes.kindHint"),
            t("network.lanes.channelCountHint"),
            t("network.lanes.sampleHint"),
            t("network.lanes.row.setNodeHint"),
            // 直连行为什么没有"钉住节点"控件（原来写在**每一行**的单元格里，N 条直连就是 N 遍）
            t("network.lanes.row.directNoGroup"),
            // 测速的口径（[19 §2.4]）：**没有装配内核时后端会如实报错**，这里也如实显示
            t("network.lanes.testHint"),
          )}
          testId="network-lanes-help"
        />
      }
      testId="network-lanes"
    >
      {/* 这里原来有一条「已选 N 条 / 删除选中」的批量条（**第 2 轮删除**）：线路不是用户维护的清单
          —— 它由后端在服务商页选「网络代理」时按需建出，一屏通常 1–3 条，而行内 `⋮` 里已经有
          「删除」。要用户先学会"勾选 → 一次确认 → 逐条调用"只为了删两条，是形式大于功能（规则 3：
          一屏一个主操作）。删掉之后线路表**恒为 6 列**（勾选框列不再出现）。
          订阅表那一条批量保留：订阅是用户自己维护的清单，可以有很多份。 */}
      {error !== undefined && error !== null ? (
        <Stack direction="row" spacing={2} alignItems="center" sx={{ flexWrap: "wrap" }}>
          <Typography variant="body1" data-testid="network-lanes-error" sx={{ color: "error.main" }}>
            {errorMessage(error)}
          </Typography>
          <Button variant="outlined" onClick={onRetry}>
            {t("common.retry")}
          </Button>
        </Stack>
      ) : rows === undefined ? (
        <Typography variant="body1" sx={{ color: "text.secondary" }}>
          {isLoading ? t("common.loading") : DASH}
        </Typography>
      ) : rows.length === 0 ? (
        // 空态：**1 行 + 1 个按钮**（规则 5）。按钮只在数据层给了地址时出现
        // （展示层不碰路由上下文，同 `subscriptionHref` 的理由）。
        <Stack direction="row" spacing={3} alignItems="center" sx={{ flexWrap: "wrap" }}>
          <Typography variant="body1" data-testid="network-lanes-empty">
            {t("network.lanes.empty")}
          </Typography>
          {providersHref === undefined ? null : (
            <Box data-testid="network-lanes-empty-goto">
              <Button variant="outlined" component={Link} to={providersHref}>
                {t("network.gotoProviders")}
              </Button>
            </Box>
          )}
        </Stack>
      ) : (
        <Box sx={TABLE_WRAP_SX}>
          {/*
            * 第 326 轮：11 列砍到 6 列（规则 4 的"默认 ≤6 列"对每一张表都成立）。
            * 留在这里的是"扫一眼要看的"：名称、类型、节点（配置里指定的那个）、启用、
            * 健康（圆点 + 延迟合成一格）、操作（一个 `⋮` 溢出菜单）。
            * 第 2 轮删掉了批量勾选框列 → 6 列是**恒定**的，不再随条数变成 7 列。
            *
            * 砍掉的组名 / 收窄正则 / 节点数 / 通道数**没有消失**：组名与收窄正则在
            * 「节点列表」页的「每条线路声明的节点来源」表里逐条列出（页头就有入口），
            * 通道数在删除确认的"谁在挡"清单里按条算出（算不出来就不写）。
            */}
          <Table size="small" data-testid="network-lanes-table">
            <TableHead>
              <TableRow>
                <TableCell sx={CELL_NOWRAP_SX}>{t("network.lanes.col.name")}</TableCell>
                <TableCell sx={CELL_NOWRAP_SX}>{t("network.lanes.col.kind")}</TableCell>
                <TableCell sx={CELL_NOWRAP_SX}>{t("network.lanes.col.node")}</TableCell>
                <TableCell sx={CELL_NOWRAP_SX}>{t("network.lanes.col.enabled")}</TableCell>
                <TableCell sx={CELL_NOWRAP_SX}>{t("network.lanes.col.health")}</TableCell>
                <TableCell align="right" sx={CELL_NOWRAP_SX}>
                  {t("network.lanes.col.actions")}
                </TableCell>
              </TableRow>
            </TableHead>
            <TableBody>
              {rows.map((row) => {
                const kindLabel = laneKindLabel(row.kind);
                return (
                  <TableRow key={row.id} hover data-testid="network-lane-row">
                    <TableCell sx={CELL_NOWRAP_SX}>
                      <AutoEllipsis testId="network-lane-name-ellipsis" sx={{ maxWidth: 200 }}>
                        {row.name === undefined ? row.id : row.name}
                      </AutoEllipsis>
                    </TableCell>
                    <TableCell sx={CELL_NOWRAP_SX}>
                      {row.kind === undefined
                        ? DASH
                        : kindLabel === undefined
                          ? // 契约外的类型原样显示（后端说的话，不改写）
                            row.kind
                          : t(kindLabel)}
                    </TableCell>
                    <TableCell sx={CELL_NOWRAP_SX}>
                      {/* [docs/26 §2] 节点名来自订阅，长名字很常见（机场里带序号/地区后缀）。
                          恢复路径：节点清单在 `/network/nodes`（页头就有入口）里有全名，
                          这里悬停/聚焦的 tooltip 也能看全。 */}
                      <AutoEllipsis testId="network-lane-node-ellipsis" sx={{ maxWidth: 240 }}>
                        {row.node ?? DASH}
                      </AutoEllipsis>
                    </TableCell>
                    <TableCell sx={CELL_NOWRAP_SX}>
                      {row.enabled ? t("common.enabled") : t("common.disabled")}
                    </TableCell>
                    {/* 健康（圆点 + 文字）与延迟合成**一格**：两个不同的列说同一件事
                        （这次采样好不好、多快），分两列只是多消耗一份横向宽度 */}
                    <TableCell sx={CELL_NOWRAP_SX}>
                      <Stack direction="row" spacing={2} alignItems="center">
                        <HealthCell health={row.health} raw={row.healthRaw} />
                        <Typography
                          variant="caption"
                          component="span"
                          sx={{ color: "text.secondary", fontVariantNumeric: "tabular-nums" }}
                        >
                          {row.latencyMs === undefined ? DASH : formatLatency(row.latencyMs)}
                        </Typography>
                      </Stack>
                    </TableCell>
                    <TableCell align="right" sx={CELL_NOWRAP_SX}>
                      {/*
                       * **一格只放一个控件**（判据 B）：原来是 160px 的节点输入框 + 钉住 + 测速 + 删除
                       * 四个控件挤在这一格里 —— 真机上输入框和「钉住」折到了第二行，整行高错位。
                       * 现在这里只有一个 `⋮`：钉住节点（含输入框）/ 测速 / 删除都进菜单
                       * （菜单挂在表格下面，见它自己的注释）。
                       *
                       * `disabled={busy}` 保留原语义：有写操作在飞时不许再开菜单发第二条命令。
                       */}
                      <IconButton
                        size="small"
                        data-testid="network-lanes-actions"
                        aria-label={`${t("network.lanes.col.actions")}：${row.name ?? row.id}`}
                        aria-haspopup="menu"
                        aria-expanded={menuRow?.id === row.id}
                        disabled={busy}
                        onClick={(event) => openMenu(event, row)}
                      >
                        <MoreVertIcon fontSize="small" />
                      </IconButton>
                    </TableCell>
                  </TableRow>
                );
              })}
            </TableBody>
          </Table>
        </Box>
      )}

      {/*
       * 行内动作的 `⋮` 溢出菜单（判据 B：一个单元格只放一个控件，行不再被折成两行）。
       *
       * 菜单**整块共用一个实例**：同一时刻只开一个，`menuRow` 是被点的那一行。内容三样，
       * 与原来那一格里的四个控件一一对应（testid 一个都没删，只是打开菜单后才进 DOM）：
       * 1. 「钉住节点」= 输入框 + 钉住按钮（`network-lanes-set-node` / `-set-node-submit`）；
       *    直连行没有这一项（它没有可切换的组 —— 那句话在本区块的问号里）。
       * 2. 「测速」（`network-lanes-test`）；3.「删除」（`network-lanes-delete`，仍走二次确认）。
       *
       * 执行任何一个动作都**先把菜单收起来**：结果区在表格下面，菜单开着会挡着它。
       */}
      <Menu
        open={menuRow !== undefined}
        anchorEl={menuAnchor}
        onClose={closeMenu}
        data-testid="network-lanes-menu"
        slotProps={{ list: { "aria-label": t("network.lanes.col.actions") } }}
      >
        {menuRow === undefined || menuRow.kind === "direct" ? null : (
          <Box
            component="li"
            data-testid="network-lanes-set-node"
            // `<li>` 直属于菜单的 `<ul>`（MenuList 的根就是它）：`div` 塞进 `ul` 是非法嵌套。
            sx={{
              listStyle: "none",
              px: 2,
              py: 1,
              display: "flex",
              alignItems: "center",
              gap: 2,
              minWidth: 280,
            }}
          >
            <TextField
              size="small"
              label={t("network.lanes.row.setNode")}
              value={nodeDrafts[menuRow.id] ?? ""}
              onChange={(event) =>
                setNodeDrafts((previous) => ({ ...previous, [menuRow.id]: event.target.value }))
              }
            />
            <Button
              size="small"
              variant="outlined"
              data-testid="network-lanes-set-node-submit"
              disabled={busy}
              onClick={() => {
                const id = menuRow.id;
                closeMenu();
                onSetNode(id);
              }}
            >
              {t("network.lanes.row.setNodeSubmit")}
            </Button>
          </Box>
        )}
        <MenuItem
          data-testid="network-lanes-test"
          disabled={busy}
          onClick={() => {
            const id = menuRow?.id;
            closeMenu();
            if (id !== undefined) onTest(id);
          }}
        >
          {t("network.lanes.test")}
        </MenuItem>
        <MenuItem
          data-testid="network-lanes-delete"
          disabled={busy}
          onClick={() => {
            const row = menuRow;
            closeMenu();
            if (row !== undefined) onDelete(row);
          }}
        >
          {t("network.lanes.row.delete")}
        </MenuItem>
      </Menu>

      {/* 行级写操作的结果：钉住成功 = 后端接受了这个名字（失败会带 TA 码原样显示） */}
      {pinned === undefined ? null : (
        <Typography variant="body1" data-testid="network-lanes-set-node-result" sx={{ overflowWrap: "anywhere" }}>
          {t("network.lanes.row.setNodeResult", { lane: pinned.id, node: pinned.node })}
        </Typography>
      )}

      {/* 删除结果（[docs/21 §1.1 第 4 条] 的 `network-lanes-delete-result`）：
          成功写"已删除 <名称>"；批量先给成功/失败条数，失败项**逐条**带 id + 后端原文。 */}
      {deleteResult.length === 0 ? null : (
        <Stack spacing={1} data-testid="network-lanes-delete-result">
          {deleteResult.map((line, index) => (
            <Typography key={`${index}-${line}`} variant="body1" sx={{ overflowWrap: "anywhere" }}>
              {line}
            </Typography>
          ))}
        </Stack>
      )}

      {/* 逐节点测速：成功逐行列延迟；失败（例如没装配内核）**原样显示后端原文** */}
      {tested === undefined ? null : (
        <Typography
          variant="body1"
          data-testid="network-lanes-test-result"
          sx={{ overflowWrap: "anywhere", color: testFailed ? "error.main" : "text.primary" }}
        >
          {testFailed
            ? `${t("network.lanes.test")} ${tested}：${errorMessage(testLane.error)}`
            : `${t("network.lanes.test")} ${tested}：${
                testLane.data === undefined || testLane.data.length === 0
                  ? DASH
                  : testLane.data
                      .map((row) =>
                        row.latency_ms === null || row.latency_ms === undefined
                          ? row.node
                          : `${row.node} ${formatLatency(row.latency_ms)}`,
                      )
                      .join(" · ")
              }`}
        </Typography>
      )}

      <FormNotice testId="network-lanes-row-error" message={rowError} />
      <WriteError testId="network-lanes-row-error" error={setNode.error ?? removeOne.error} />

      {/* 删除确认（[docs/21 §1 / §2]）：**先确认再发命令**；被引用时后端原文原样显示。
          `lane_delete` 没有 force 语义 → `cascadeAvailable={false}`，一个级联按钮都不放。 */}
      <ConfirmDelete
        open={deleteTarget !== undefined}
        testIdPrefix="network-lanes"
        title={t("delete.title.lane")}
        // 确认框里写清删的是哪一个：线路只有单条删除（批量随勾选框列一起删了）→ 名称 + id
        targetLabel={deleteTarget === undefined ? "" : (deleteTarget.labels[0] ?? "")}
        consequences={deleteTarget?.consequences}
        cascadeAvailable={false}
        pending={removeOne.isPending}
        errorText={deleteError}
        onConfirm={() => void confirmDelete()}
        onConfirmCascade={() => {}}
        onCancel={() => {
          setDeleteTarget(undefined);
          setDeleteError(undefined);
        }}
      />

      {/* [docs/58] 这里原来是「添加线路 / 编辑线路」的对话框 —— 手动建线路的整套界面已删除。
          线路在服务商页被选中时由后端按需建出来（见本区块上方的说明）。 */}
    </SectionPaper>
  );
}

// ================================================================ 展示层（纯 props，便于测"有数据/无数据/读不到"）

export type NetworkBodyProps = {
  status: AppStatus | undefined;
  /**
   * `net.kernel.path`（来自 `config_get`）。
   * `undefined` = **还没读到** —— 与"读到了、但为空（用户没配内核路径）"不是一回事（[18 §2.1]）。
   * 空值时出口显示「出口 · 直连」，**不是**错误态（[19 §2.2]）。
   */
  kernelPath: string | undefined;
  subscriptions: SubscriptionRow[] | undefined;
  subscriptionsLoading: boolean;
  subscriptionsError: unknown;
  lanes: LaneRow[] | undefined;
  lanesLoading: boolean;
  lanesError: unknown;
  onRetry: () => void;
  /**
   * 是否显示「从服务商向导返回」的返回条（[19 §1.4]）。
   *
   * 由**数据层**从真实 URL 的 `?from=provider-wizard` 读出来传进来（`NetworkBody` 是纯 props 展示层，
   * 不自己解 URL —— 那样展示层的单测就都得包一层 Router）。
   */
  showProviderReturn?: boolean;
  /**
   * 「点订阅名进详情页」的地址（[docs/54]）。由数据层给，见 `SubscriptionsSection` 的 prop 注释。
   * 不传就退化成纯文本（展示层单测不包 Router）。
   */
  subscriptionHref?: (id: string) => string;
  /** 线路空态里「去服务商页」按钮的地址（同上：路由地址由数据层给）。 */
  providersHref?: string;
};

/**
 * 网络概览的展示层（纯 props，便于测"有数据 / 空 / 读不到"三种状态）。
 *
 * **[19 §2.2] 之后这里没有任何"门控"**：网络代理默认开启（默认直连），所以这一页
 * **永远**渲染出口 / 订阅 / 线路三块 —— 没配置时每块给**诚实的空态**（不是空表、不是白纸），
 * 但页面上**没有"启用网络代理"这个动作**（上一版的 `network-optional` 已作废）。
 */
export function NetworkBody({
  status,
  kernelPath,
  subscriptions,
  subscriptionsLoading,
  subscriptionsError,
  lanes,
  lanesLoading,
  lanesError,
  onRetry,
  showProviderReturn = false,
  subscriptionHref,
  providersHref,
}: NetworkBodyProps) {
  return (
    <Box sx={{ px: 8, pb: 8, display: "flex", flexDirection: "column", gap: 6 }}>
      {showProviderReturn ? <ProviderReturnBanner /> : null}
      {/* 零副作用承诺不再单独占一张卡片（它随页头那个问号走，见文件头那段说明） */}
      <CurrentExit status={status} lanes={lanes ?? []} kernelPath={kernelPath} />
      <SubscriptionsSection
        rows={subscriptions}
        // 线路列表在这里只是"这份订阅被谁引用"的证据来源（删除确认里的连带清单）
        laneRows={lanes}
        isLoading={subscriptionsLoading}
        error={subscriptionsError}
        onRetry={onRetry}
        subscriptionHref={subscriptionHref}
      />
      <LanesSection
        rows={lanes}
        isLoading={lanesLoading}
        error={lanesError}
        onRetry={onRetry}
        providersHref={providersHref}
      />
    </Box>
  );
}

// ================================================================ 数据层

export default function NetworkPage() {
  const { t } = useTranslation();
  // 返回条的触发条件是**真实 URL**（[19 §1.4]）：服务商向导里的「让我先设置网络代理」
  // 跳到 `#/network?from=provider-wizard`，这里据此给一条回得去的路。
  const [searchParams] = useSearchParams();
  const showProviderReturn = searchParams.get("from") === "provider-wizard";
  // `app_status` 的唯一取数入口（与状态条共用同一份缓存）
  const status = useAppStatus({ refetchInterval: 30_000 });
  const subscriptions = useCommandQuery("subscription_list", undefined, qk.subscriptions);
  const lanes = useCommandQuery("lane_list", undefined, qk.lanes);
  // 出口的「直连 / 线路名」判定要用 `net.kernel.path`（[19 §2.2]）：它只在 `config_get` 里，
  // 而 `config_get` 是**原子快照**（与分配页共用同一份缓存，`config:changed` 事件会失效它）。
  const config = useCommandQuery("config_get", undefined, qk.config);

  const refresh = () => {
    void status.refetch();
    void subscriptions.refetch();
    void lanes.refetch();
    void config.refetch();
  };

  return (
    <>
      <PageHeader
        title={t("nav.network")}
        // 页头那句说明 + I2 的零副作用承诺合进**同一个**问号（承诺原来占一整张卡片，见文件头）
        description={mergeHelpText(
          t("network.description"),
          t("network.zeroSideEffect.title"),
          t("network.zeroSideEffect.body"),
        )}
        actions={
          <>
            <Button variant="outlined" onClick={refresh}>
              {t("common.refresh")}
            </Button>
            <Button variant="outlined" component={Link} to="/network/nodes">
              {t("network.gotoNodes")}
            </Button>
            {/* 「线路分配」那一页整页删除了（[docs/55]）→ 这里的入口一并去掉：
                留一个点了落到不存在路由的按钮，就是一个假控件。 */}
          </>
        }
      />
      <NetworkBody
        status={status.data}
        kernelPath={config.data === undefined ? undefined : readKernelPath(config.data)}
        // 订阅行只带**配置与解析结果**：流量（运行时观测值）现在归详情页自己读（[18 §1.2]）
        subscriptions={
          subscriptions.data === undefined
            ? undefined
            : subscriptions.data.map((item) => readSubscription(item))
        }
        subscriptionsLoading={subscriptions.isPending}
        subscriptionsError={subscriptions.error ?? undefined}
        lanes={lanes.data === undefined ? undefined : lanes.data.map((item) => readLane(item))}
        lanesLoading={lanes.isPending}
        lanesError={lanes.error ?? undefined}
        onRetry={refresh}
        showProviderReturn={showProviderReturn}
        // [docs/54] 数据层负责拼地址（展示层不碰路由上下文）
        subscriptionHref={(id) => `/network/subscriptions/${encodeURIComponent(id)}`}
        // 线路空态那个按钮的地址（同一个理由：展示层不碰路由上下文）
        providersHref="/providers"
      />
    </>
  );
}
