/**
 * 服务商列表 `/providers`—— M1 里"配了哪几家、还活着吗"的入口。
 *
 * 三条硬纪律（写在最上面，这一页最容易违反）：
 *
 * 1. **只调用已注册的命令**：本页取数只用 `provider_list` / `credential_list`；
 *    写入口是页头的**两个**向导（见下），它们调 `provider_upsert` / `credential_upsert` /
 *    `credential_set_secret` / `model_apply_import` —— 都在 `src-tauri/src/commands/mod.rs` 的
 *    `collect_commands!` 里（`src/app/commandSurface.guard.test.ts` 机械检查）。
 *    **本页已有真入口**：`providers-add-open`（两步向导： 服务商信息 → 创建线路，
 *    `./providerWizard.tsx`）、`providers-add-advanced-open`（**原样的单步对话框**，
 *    `./wizards.tsx`，一次填完 id / 鉴权方式等完整字段）与 `providers-delete`（删除，
 *    走共享确认组件 `ConfirmDelete`，先 `force=false`、被引用才给级联）。**编辑入口就是"点名称
 *    进详情页"**（：详情页本身是查看 + 编辑一体的编辑器），所以行上不需要第二个编辑按钮；
 *    连通性测试仍未接线，未接线之前不放假控件、也不放禁用占位。
 *    **本页没有凭据表，也没有「添加密钥」入口**（硬规则 3：**一个服务商一把密钥**；
 *    要两个账号就**再加一条服务商、取不同名字**）。凭据只用来数"挂载了几个"（见下第 3 条），
 *    它的写入口全在别处：两步向导第 1 步写 `<provider>-default` 那一把、详情页「密钥」块改
 *    已有的那一把（留空 = 不改）。所以**列表页一个 `credential_upsert` 调用点都没有** ——
 *    `providers.test.tsx` ⑦ 用 recorder 数调用次数 = 0 钉住（第 73 轮那条"添加密钥"入口已下线，
 *    它正是"给一个已存在的服务商再挂一份凭据"那个被 owner 否掉的模型）。
 * 2. **没有读取接口的就不猜**：状态灯、成功率、延迟、通道数都**只**取后端返回的那一列；
 *    后端没给（Rust 的 `provider_list` 返回的是完整 `Provider`，没有这几列）就显示「未知/—」。
 *    内核在跑**不等于**服务商可用（B2），所以绝不点一个假绿灯。
 * 3. **凭据数读不到就是未知**：`credential_list` 失败只让"凭据数"这一列变未知 + 一行错误，
 *    **不把整张表打成错误**，也绝不填 0。
 *
 * 数据流（哪个门面 → 哪个命令）：
 * - `useCommandQuery("provider_list", undefined, qk.providers)`   → 目录（详情页共用同一个 key）
 * - `useCommandQuery("credential_list", undefined, qk.credentials)`→ 凭据（用来数"挂载了几个凭据"）
 * - `./wizards.tsx` 的高级添加在成功后失效 `qk.providers`；两步向导（`./providerWizard.tsx`）
 *   在第 1 步落库后失效 `qk.providers` / `qk.credentials`
 *
 * 契约与 Rust 的字段偏差、以及"为什么详情页也从 `provider_list` 取行"见 `model.ts` 的文件头。
 *
 * ## 本轮（极简改造）在这一页上改了四处
 *
 * 1. **空态只有 1 行**（规则 5 + 第 2 轮补丁判据 A）：原来那句"服务商来自配置文件的
 *    providers 段……写接口尚未接线"整条删掉 —— 它既不是状态也不是动作，还把"还没接线"
 *    写死在界面上。空态**不放按钮**：页头那个「添加服务商」是同一个动作，重复一个就是
 *    两个 primary 说同一件事（`/apikeys` 已是对的形态）。
 * 2. **多选按行数收敛**（规则 3）：只有 1 个服务商时不再画复选框与「已选 0 条 / 删除选中」
 *    （行内的「删除」已经覆盖了唯一的需求）。
 * 3. **表格列收敛**（规则 4）：「启用」并入「状态」一列（都是"现在是什么状态"），
 *    并给所有单元格加 `whiteSpace: nowrap` —— 窄窗口下横向滚动，不把表头挤成竖排单字。
 * 4. **长句与实现细节下版面**（规则 1/2）：凭据读失败那半句解释、"后端返回的 status 列"、
 *    "provider 字段"这些词全部离开版面；能留的收进标题右边**那一个** `?`。
 */

import { Box, Button, Checkbox, Link, Paper, Stack, Table, TableBody, TableCell, TableHead, TableRow, Typography } from "@mui/material";
import { useQueryClient } from "@tanstack/react-query";
import { useMemo, useState } from "react";
import { useNavigate } from "react-router-dom";

import { errorMessage, qk, useCommandMutation, useCommandQuery } from "@/shared/api";
import { SECTION_SX, Caption, AutoEllipsis, ConfirmDelete, HelpTip, PageHeader, StatusDot, type SemStatus } from "@/shared/components";
import { DASH, formatInt, formatLatency, formatPercent } from "@/shared/format";
import { useTranslation, type I18nKey } from "@/shared/i18n";
import { typography } from "@/theme";

import { mountOf, readCredential, readProvider, type CredentialRow, type ProviderRow } from "./model";
import { ProviderTwoStepWizard } from "./providerWizard";
import { readWizardSession, resetWizardSession, writeWizardSession } from "./providerWizardModel";
import { ProviderAddDialog } from "./wizards";

// ================================================================ 文案映射（显式表，不用模板拼 key）

/** 协议 → 界面语言。**不把 `openai_responses` 这类线上值漏给用户。** */
const PROTOCOL_LABEL: Record<string, I18nKey> = {
  openai: "providers.protocol.openai",
  openai_responses: "providers.protocol.openai_responses",
  anthropic: "providers.protocol.anthropic",
  gemini: "providers.protocol.gemini",
  custom: "providers.protocol.custom",
};

/** 后端 `status` 的取值 → 全站统一的状态语义。契约外的取值一律按"未知"画。 */
const STATUS_SEM: Record<string, SemStatus> = { ok: "ok", warn: "warn", error: "error", off: "off" };
const STATUS_LABEL: Record<string, I18nKey> = {
  ok: "component.statusDot.ok",
  warn: "component.statusDot.warn",
  error: "component.statusDot.error",
  off: "component.statusDot.off",
};

/** 数值缺失一律画「—」（**不画 0**：0 是"确实是 0"，缺失是"不知道"）。 */


/**
 * 「高级添加」入口点开的是**原样的单步对话框**（字段 tid 全不变）。
 *
 * 文案走 i18n 键 `providers.add.advanced`（第 73 轮主管已补上）：界面上的每个字都必须能在
 * `src/shared/i18n/**` 里找到，页面里的中文字面量是漏翻的入口（英文界面会露中文）。
 */

/**
 * 单元格「不换行」：版面上的行**不许**被长内容（例如「1 个凭据 · 3 条通道」）撑成多行。
 *
 * 窄窗口下宁可让 `<Box sx={{ overflowX: "auto" }}>` 横向滚动，也不把表头/行挤成竖排单字。
 */
const CELL_SX = { whiteSpace: "nowrap" } as const;

// ================================================================ 删除

/**
 * 「会连带删除 / 影响」清单的行文案：用 i18n 键 `delete.consequence.credentials` / `.channels`
 * （第 73 轮主管补上；在这之前这里是带注释的字面量，英文界面会露中文）。
 *
 * **只放前端真算得出来的事实**：凭据数来自 `credential_list`、通道数来自 `provider_list` 的
 * `channel_count` 那一列；两样任一读不到就不写那一行（：算不出来不编）。
 * 本页**读不到策略清单**，所以这里不会出现"被 N 条策略引用"这类话。
 */

/**
 * 表头全选复选框的无障碍标签。
 *
 * 用 `common.selectAll`（主管补的通用键）；此前复用向导里的 `providers.wizard.selectAll`，
 * 那句话带"向导"的语境，放在列表页的表头上并不贴切。
 */
const SELECT_ALL_LABEL_KEY: I18nKey = "common.selectAll";

/** 一次删除的目标（单条与批量共用同一个形状，语义也共用同一套）。 */
type DeleteTarget = {
  /** 要删的 id：单条 = 一个；批量 = 选中的那些（顺序 = 选中顺序）。 */
  readonly ids: readonly string[];
  /** 确认框里写清的"删的是哪一个"（名称 + id；批量 = 逐条列出）。 */
  readonly label: string;
  /** 批量与单条唯一的区别：批量**没有**级联按钮（规定批量一律 `force=false`）。 */
  readonly batch: boolean;
  /** 前端算得出来的连带影响（算不出来就是空数组）。 */
  readonly consequences: readonly string[];
};

/**
 * 一次删除执行完的**事实**：哪些真删掉了、哪些失败了、后端各自说了什么。
 *
 * 故意**不带任何文案** —— 这一层只管"调了什么、后端回了什么"，文案由页面用 `t(...)` 拼
 * （页面的 `t` 有完整的键类型，模块级函数拿不到那个类型）。
 */
type DeleteOutcome = {
  deletedIds: string[];
  /** 逐条失败：id + 后端原文（含 TA 码）。 */
  failures: { id: string; message: string }[];
};

/**
 * 执行一次删除（单条或批量），**同一套语义**：
 *
 * 1. 逐条调 `remove(id, force)`；**批量一律 `force=false`**（批量没有级联档，机械地钉在这里）；
 * 2. **一条失败不中止其余条**，逐条记录结果；
 * 3. 绝不吞错误：失败项的 `id` 与后端原文一起交回调用方（批量逐条汇报、单条进对话框）。
 */
async function runDelete(
  target: DeleteTarget,
  force: boolean,
  remove: (id: string, force: boolean) => Promise<unknown>,
): Promise<DeleteOutcome> {
  const effectiveForce = target.batch ? false : force;
  const deletedIds: string[] = [];
  const failures: { id: string; message: string }[] = [];

  for (const id of target.ids) {
    try {
      await remove(id, effectiveForce);
      deletedIds.push(id);
    } catch (err) {
      failures.push({ id, message: errorMessage(err) });
    }
  }

  return { deletedIds, failures };
}

// ================================================================ 小组件

/** 弱化的一行说明（诚实性说明专用）。 */

// ================================================================ 展示层（纯 props，便于测"有数据/无数据"）

export type ProvidersBodyProps = {
  rows: ProviderRow[];
  isLoading: boolean;
  error: unknown;
  onRetry: () => void;
  /**
   * 凭据列表。**`undefined` = 还没读回来或读失败** → 凭据数显示"未知"（不是 0）。
   * 空数组才是"确实一个凭据都没有"。
   */
  credentials: CredentialRow[] | undefined;
  credentialsError: unknown;
  onRetryCredentials: () => void;
  /** 打开某一个服务商（页面注入路由跳转，展示层不认识 router）。 */
  onOpen: (id: string) => void;
  /**
   * 行内「删除」。**省略 = 这一行没有删除入口** ——
   * 纯展示件不自己造写入口，页面（数据层）注入真处理器。
   */
  onDelete?: (row: ProviderRow) => void;
  /** 多选（行 `providers-select` / 表头 `providers-select-all` / 计数 `providers-selected-count`）。 */
  selectedIds?: readonly string[];
  onToggleSelect?: (id: string) => void;
  /** 表头全选：把**当前看得见的那批 id**交回数据层（谁可见由展示层说了算）。 */
  onToggleSelectAll?: (visibleIds: readonly string[]) => void;
  /** 「删除选中」（`providers-delete-selected`）：一次确认后**逐条**删。 */
  onDeleteSelected?: () => void;
};

export function ProvidersBody({
  rows,
  isLoading,
  error,
  onRetry,
  credentials,
  credentialsError,
  onRetryCredentials,
  onOpen,
  onDelete,
  selectedIds,
  onToggleSelect,
  onToggleSelectAll,
  onDeleteSelected,
}: ProvidersBodyProps) {
  const { t } = useTranslation();

  const selected = selectedIds ?? [];
  const selectedSet = useMemo(() => new Set(selected), [selected]);
  /**
   * 多选只在**真的有多选场景**时才画（规则 3）：数据层注入两个处理器 + **列表里不止一行**。
   *
   * 只有 1 个服务商时，行内「删除」已经覆盖了唯一的删除需求 —— 再摆一个「已选 0 条 / 删除选中」
   * 就是让用户先勾一个再点删除（同一件事两条路，而且多出一次点击）。0 行时更不该出现。
   */
  const selectable =
    onToggleSelect !== undefined && onToggleSelectAll !== undefined && rows.length > 1;
  const multiSelect = selectable && onDeleteSelected !== undefined;
  const visibleIds = rows.map((row) => row.id);
  const allSelected = visibleIds.length > 0 && visibleIds.every((id) => selectedSet.has(id));
  const someSelected = !allSelected && visibleIds.some((id) => selectedSet.has(id));

  /** 挂载文案：**两半各自独立**，哪半读不到就说哪半未知（不写 `0 / —` 这种含糊写法）。 */
  const mountText = (row: ProviderRow): string => {
    const mount = mountOf(row, credentials);
    if (mount.credentials !== undefined && mount.channels !== undefined) {
      return t("providers.mount.both", {
        credentials: formatInt(mount.credentials),
        channels: formatInt(mount.channels),
      });
    }
    if (mount.credentials !== undefined) {
      return t("providers.mount.credentialsOnly", { credentials: formatInt(mount.credentials) });
    }
    if (mount.channels !== undefined) {
      return t("providers.mount.channelsOnly", { channels: formatInt(mount.channels) });
    }
    return t("providers.mount.unknown");
  };

  return (
    <Box sx={{ px: 8, pb: 8, display: "flex", flexDirection: "column", gap: 6 }}>
      <Paper variant="outlined" sx={SECTION_SX}>
        <Stack spacing={3}>
          {/* 表名 + 条数。**条数是数值→ 留在版面上**，
              而这一块原来铺在表下面的四行长解释（状态灯列从哪来 / 缺列为什么画「—」/
              凭据数怎么归并 / 点名称去哪）收进标题右边的**一个**问号里：
              正文一个字都没丢，`aria-label` 就是全文。 */}
          <Box sx={{ display: "flex", alignItems: "baseline", gap: 3, flexWrap: "wrap" }}>
            <Box sx={{ display: "flex", alignItems: "center", gap: 2, minWidth: 0 }}>
              <Typography variant="h4">{t("providers.tableTitle")}</Typography>
              <HelpTip
                text={[
                  t("providers.honesty.status"),
                  t("providers.honesty.fields"),
                  t("providers.honesty.mounted"),
                  t("providers.honesty.click"),
                ].join("\n")}
                testId="providers-table-help"
              />
            </Box>
            {!isLoading && error === undefined ? (
              <Caption>{t("providers.count", { value: formatInt(rows.length) })}</Caption>
            ) : null}
          </Box>

          {/* 凭据读失败只降级"凭据数"这一列：说清原因（带 TA 码）+ 单独重试，不毁整张表 */}
          {credentialsError !== undefined && credentialsError !== null ? (
            <Stack direction="row" spacing={2} alignItems="center" sx={{ flexWrap: "wrap" }}>
              <Typography
                variant="caption"
                data-testid="providers-credentials-error"
                sx={{ color: "error.main" }}
              >
                {t("providers.credentialsError", { message: errorMessage(credentialsError) })}
              </Typography>
              <Button variant="outlined" onClick={onRetryCredentials}>
                {t("common.retry")}
              </Button>
            </Stack>
          ) : null}

          {error !== undefined && error !== null ? (
            <Stack direction="row" spacing={2} alignItems="center" sx={{ flexWrap: "wrap" }}>
              {/* `errorMessage()` 自带 `[TA-xxxx]`（唯一格式化出口），不吞错误码 */}
              <Typography variant="body1" data-testid="providers-error" sx={{ color: "error.main" }}>
                {t("providers.error", { message: errorMessage(error) })}
              </Typography>
              <Button variant="outlined" onClick={onRetry}>
                {t("common.retry")}
              </Button>
            </Stack>
          ) : isLoading ? (
            <Typography variant="body1" sx={{ color: "text.secondary" }}>
              {t("common.loading")}
            </Typography>
          ) : rows.length === 0 ? (
            /* 空态（补丁判据 A + 规则 5）：**只有 1 行说明**，不放按钮。
               页头已经有同一个「添加服务商」（同标签同动作）—— 空态里再放一个就是
               两个 primary 说同一件事（`/apikeys` 就是这个正确形态）。
               「服务商来自配置文件的 providers 段 / 写接口还没接线」那种解释整条删掉：
               它既不是状态也不是动作，还把"还没接线"写死在了界面上。 */
            <Typography variant="body1" data-testid="providers-empty">
              {t("providers.empty.title")}
            </Typography>
          ) : (
            <>
              {/* 删除选中：只在**多选真的出现**时才有这一行（见 `multiSelect`），
                  一行都没选时按钮点不动，也就不会去开后端根本不需要受理的批删。 */}
              {!multiSelect ? null : (
                <Stack direction="row" spacing={2} alignItems="center" sx={{ flexWrap: "wrap" }}>
                  <Typography variant="caption" data-testid="providers-selected-count">
                    {t("delete.selectedCount", { count: selected.length })}
                  </Typography>
                  <Button
                    variant="outlined"
                    color="error"
                    data-testid="providers-delete-selected"
                    disabled={selected.length === 0}
                    onClick={onDeleteSelected}
                  >
                    {t("common.deleteSelected")}
                  </Button>
                </Stack>
              )}

              <Box sx={{ overflowX: "auto" }}>
                <Table size="small" data-testid="providers-table">
                  <TableHead>
                    <TableRow>
                      {selectable ? (
                        <TableCell padding="checkbox">
                          <Checkbox
                            data-testid="providers-select-all"
                            checked={allSelected}
                            indeterminate={someSelected}
                            onChange={() => onToggleSelectAll?.(visibleIds)}
                            slotProps={{ input: { "aria-label": t(SELECT_ALL_LABEL_KEY) } }}                          />
                        </TableCell>
                      ) : null}
                      <TableCell sx={CELL_SX}>{t("providers.col.name")}</TableCell>
                      <TableCell sx={CELL_SX}>{t("providers.col.protocol")}</TableCell>
                      {/* 「状态」= 健康灯 + 启用开关的**合并列**（规则 4：内容列 ≤6）。
                          两列说的都是"这家现在是什么状态"，各占一列会让表头在窄窗口里竖排。 */}
                      <TableCell sx={CELL_SX}>{t("providers.col.status")}</TableCell>
                      <TableCell align="right" sx={{ ...CELL_SX, fontVariantNumeric: "tabular-nums" }}>
                        {t("providers.col.successRate")}
                      </TableCell>
                      <TableCell align="right" sx={{ ...CELL_SX, fontVariantNumeric: "tabular-nums" }}>
                        {t("providers.col.latency")}
                      </TableCell>
                      <TableCell sx={CELL_SX}>{t("providers.col.mount")}</TableCell>
                      {/* 操作列表头留空（与 `pages/apikeys` 同一写法）：没有现成的键，
                          不为一个表头去造键（键由主管独占）。 */}
                      {onDelete === undefined ? null : <TableCell align="right" />}
                    </TableRow>
                  </TableHead>
                  <TableBody>
                    {rows.map((row) => {
                      const status = row.status;
                      /**
                       * 一列装两件事：**关闭**优先（关掉的这条不参与路由，健康度对它没有意义），
                       * 否则画健康灯。这样表头少一列，用户在窄窗口里也不会看到竖排的表头。
                       */
                      const disabled = row.enabled === false;
                      const sem: SemStatus = disabled
                        ? "off"
                        : status === undefined
                          ? "off"
                          : (STATUS_SEM[status] ?? "off");
                      const statusLabel = disabled
                        ? t("common.disabled")
                        : status === undefined
                          ? t("common.unknown")
                          : STATUS_LABEL[status] !== undefined
                            ? t(STATUS_LABEL[status]!)
                            : // 契约外的取值原样显示（是后端说的话，不改写、不猜）
                              status;
                      const protocol = row.protocol;

                      return (
                        <TableRow key={row.id} hover data-testid="providers-row">
                          {selectable ? (
                            <TableCell padding="checkbox">
                              <Checkbox
                                data-testid="providers-select"
                                checked={selectedSet.has(row.id)}
                                onChange={() => onToggleSelect?.(row.id)}
                                slotProps={{
                                  // 无障碍标签用**这一行自己的名字**（用户数据，不需要新键）
                                  input: { "aria-label": row.name === "" ? row.id : row.name },
                                }}
                              />
                            </TableCell>
                          ) : null}
                          <TableCell sx={CELL_SX}>
                            {/* 名称是进入详情的入口。**它只是个跳转**，不是数据写入。
 省略的**两条**恢复路径都在这一格上：
                                ①点名称进详情页（本行既有入口，详情页一个字都不省略）；
                                ②悬停/聚焦的 tooltip。所以这里的省略不构成"信息被藏起来"。 */}
                            <Link
                              component="button"
                              type="button"
                              onClick={() => onOpen(row.id)}
                              sx={{
                                fontSize: typography.size.body,
                                textAlign: "left",
                                // 块级 + 撑满格子：省略需要一个确定宽度，行内盒子会走 min-content 把列撑宽
                                display: "block",
                                maxWidth: "100%",
                              }}
                            >
                              <AutoEllipsis testId="providers-name-ellipsis" sx={{ maxWidth: 240 }}>
                                {row.name === "" ? row.id : row.name}
                              </AutoEllipsis>
                            </Link>
                          </TableCell>
                          <TableCell sx={{ ...CELL_SX, color: "text.secondary" }}>
                            {protocol === undefined
                              ? DASH
                              : PROTOCOL_LABEL[protocol] !== undefined
                                ? t(PROTOCOL_LABEL[protocol]!)
                                : protocol}
                          </TableCell>
                          <TableCell sx={CELL_SX}>
                            <StatusDot status={sem} label={statusLabel} />
                          </TableCell>
                          <TableCell
                            align="right"
                            sx={{ ...CELL_SX, fontVariantNumeric: "tabular-nums" }}
                          >
                            {row.successRate24h === undefined
                              ? DASH
                              : formatPercent(row.successRate24h, 1)}
                          </TableCell>
                          <TableCell
                            align="right"
                            sx={{ ...CELL_SX, fontVariantNumeric: "tabular-nums" }}
                          >
                            {row.avgLatencyMs === undefined ? DASH : formatLatency(row.avgLatencyMs)}
                          </TableCell>
                          <TableCell sx={{ ...CELL_SX, color: "text.secondary" }}>
                            {mountText(row)}
                          </TableCell>
                          {onDelete === undefined ? null : (
                            <TableCell align="right" sx={CELL_SX}>
                              <Button
                                size="small"
                                variant="outlined"
                                color="error"
                                data-testid="providers-delete"
                                onClick={() => onDelete(row)}
                              >
                                {t("common.delete")}
                              </Button>
                            </TableCell>
                          )}
                        </TableRow>
                      );
                    })}
                  </TableBody>
                </Table>
              </Box>
            </>
          )}

          {/* 原来这里铺着四行"诚实说明"灰字（哪些列是后端真给的、哪些只是这一版没有）。
              它们全是**解释**，已按 合成一条收进标题右边的 `providers-table-help`
              （见上面那个问号）——**不是删掉**：正文进 tooltip 与 `aria-label`，一个字都没丢。 */}
        </Stack>
      </Paper>
    </Box>
  );
}

// ================================================================ 数据层

export default function ProvidersPage() {
  const { t } = useTranslation();
  const navigate = useNavigate();

  // 详情页读同一个 key：一次取数、两页共用缓存（事件到达时由事件桥失效）
  const providers = useCommandQuery("provider_list", undefined, qk.providers);
  const credentials = useCommandQuery("credential_list", undefined, qk.credentials);

  /**
   * 向导的开合。**列表页只读**，写全部落在 `./providerWizard.tsx`（两步向导）
   * 与 `./wizards.tsx`（高级添加的单步对话框）里。
   *
   * 这里**没有**「添加密钥」的开关：一个服务商一把密钥，凭据的写入口只有两步向导第 1 步
   * 与详情页「密钥」块两处（硬规则 3）—— 在列表页再开一个"给已存在的服务商
   * 挂第二份凭据"的对话框，就是 owner 明确否掉的那个模型。
   *
   * 两步向导的"开着/第几步/草稿"活在 `./providerWizardModel.ts` 的**模块级 store** 里：
   * 用户点 `pw-proxy` 去 `/network` 会把这一页整个卸载，草稿必须比组件活得久
   * 。所以初值从 store 读 —— 从 `/network` 回来时向导自己还是开着的。
   */
  const [providerAddOpen, setProviderAddOpen] = useState(() => readWizardSession().open);
  const [advancedAddOpen, setAdvancedAddOpen] = useState(false);

  const openProviderWizard = (): void => {
    writeWizardSession({ open: true });
    setProviderAddOpen(true);
  };
  const closeProviderWizard = (): void => {
    resetWizardSession();
    setProviderAddOpen(false);
  };

  const rows = useMemo(() => (providers.data ?? []).map((p) => readProvider(p)), [providers.data]);
  /** `undefined`（读不到）与 `[]`（确实是空）在这里被严格区分开。 */
  const credentialRows = useMemo(
    () => (credentials.data === undefined ? undefined : credentials.data.map((c) => readCredential(c))),
    [credentials.data],
  );

  /** 本地去重用的现有 id：契约的视图与 Rust 的完整实体都有 `id`，两种形状都读得到。 */
  const providerIds = useMemo(() => (providers.data ?? []).map((p) => p.id), [providers.data]);
  /** 两步向导的显示名称**去重**要对着后端返回的现有名字做。 */
  const providerNames = useMemo(
    () => (providers.data ?? []).map((p) => p.name).filter((name): name is string => typeof name === "string"),
    [providers.data],
  );

  const refresh = () => {
    void providers.refetch();
    void credentials.refetch();
  };

  // ---------------------------------------------------------------- 删除

  const qc = useQueryClient();
  const removeProvider = useCommandMutation("provider_delete");

  /** 多选（按 id 存，顺序 = 用户勾选顺序）。 */
  const [selectedIds, setSelectedIds] = useState<readonly string[]>([]);
  /** 确认框里那一条（`undefined` = 框没开）。 */
  const [deleteTarget, setDeleteTarget] = useState<DeleteTarget | undefined>(undefined);
  /** 后端原文（被引用时的错误）：有它才在对话框里显示、也才有级联按钮。 */
  const [deleteError, setDeleteError] = useState<string | undefined>(undefined);
  /** 结果区：**每一次事实**一行（成功 / 批量汇报 / 失败项）。 */
  const [deleteResult, setDeleteResult] = useState<readonly string[]>([]);
  /** 批量进行中（`mutateAsync` 逐条之间 `isPending` 会回落，所以单独记一笔）。 */
  const [batchRunning, setBatchRunning] = useState(false);

  const nameOf = (id: string): string => {
    const row = rows.find((candidate) => candidate.id === id);
    if (row === undefined) return id;
    return row.name === "" ? row.id : row.name;
  };

  /** 删除成功后要失效的查询：服务商级联会动到凭据 / 模型 / 通道矩阵。 */
  const invalidateAfterDelete = async (): Promise<void> => {
    for (const key of [qk.providers, qk.credentials, qk.models, qk.channelMatrix]) {
      await qc.invalidateQueries({ queryKey: key });
    }
  };

  /**
   * 「会连带删除 / 影响」清单：**只用前端算得出来的事实**。
   * 凭据数来自 `credential_list`、通道数来自 `provider_list` 的 `channel_count`；
   * 读不到的那一维**不写**（不编）。
   */
  const consequencesOf = (row: ProviderRow): string[] => {
    const mount = mountOf(row, credentialRows);
    const lines: string[] = [];
    if (mount.credentials !== undefined) {
      lines.push(t("delete.consequence.credentials", { value: formatInt(mount.credentials) }));
    }
    if (mount.channels !== undefined) {
      lines.push(t("delete.consequence.channels", { value: formatInt(mount.channels) }));
    }
    return lines;
  };

  const providerLabel = (row: ProviderRow): string =>
    row.name === "" ? row.id : `${row.name}（${row.id}）`;

  const openDelete = (target: DeleteTarget): void => {
    setDeleteTarget(target);
    setDeleteError(undefined);
    // 新一次操作开始 → 清掉上一次的汇报（结果区永远只讲刚刚发生的事）
    setDeleteResult([]);
  };

  const cancelDelete = (): void => {
    setDeleteTarget(undefined);
    setDeleteError(undefined);
  };

  const confirmDelete = async (force: boolean): Promise<void> => {
    const target = deleteTarget;
    if (target === undefined) return;
    if (target.batch) setBatchRunning(true);
    const outcome = await runDelete(target, force, (id, effectiveForce) =>
      removeProvider.mutateAsync({ id, force: effectiveForce }),
    );
    setBatchRunning(false);

    // 结果区：**只写真的发生过的事**
    // 单条失败时不写任何成功 —— 后端原文进对话框（那里才有级联按钮）。
    const singleFailed = !target.batch && outcome.failures.length > 0;
    setDeleteResult(
      target.batch
        ? [
            t("delete.result.summary", {
              ok: formatInt(outcome.deletedIds.length),
              failed: formatInt(outcome.failures.length),
            }),
            ...outcome.deletedIds.map((id) => t("delete.result.ok", { name: nameOf(id) })),
            ...outcome.failures.map((failure) =>
              t("delete.result.failed", { message: `${failure.id} · ${failure.message}` }),
            ),
          ]
        : singleFailed
          ? []
          : outcome.deletedIds.map((id) => t("delete.result.ok", { name: nameOf(id) })),
    );
    setDeleteError(singleFailed ? outcome.failures[0]!.message : undefined);
    // 只有**真的删掉**的才从选中里摘掉；失败的留着（用户可以直接再来一次）
    setSelectedIds((prev) => prev.filter((id) => !outcome.deletedIds.includes(id)));
    // 单条失败时把框留着（后端原文与级联按钮就在那里）；其余情况一律收框
    if (target.batch || !singleFailed) setDeleteTarget(undefined);
    await invalidateAfterDelete();
  };

  const toggleSelect = (id: string): void => {
    setSelectedIds((prev) =>
      prev.includes(id) ? prev.filter((selectedId) => selectedId !== id) : [...prev, id],
    );
  };

  /** 表头全选：对**当前列表里看得见的行**生效（可见性由展示层交回来）。 */
  const toggleSelectAll = (visible: readonly string[]): void => {
    setSelectedIds((prev) =>
      visible.length > 0 && visible.every((id) => prev.includes(id))
        ? prev.filter((id) => !visible.includes(id))
        : [...new Set([...prev, ...visible])],
    );
  };

  return (
    <>
      <PageHeader
        title={t("nav.providers")}
        description={t("providers.description")}
        actions={
          /*
            规则 3（一屏一个主操作）：**只有「添加服务商」是 contained**。
            「刷新」与「高级添加」降级成 text —— 它们是副操作，不该跟主操作抢同一个视觉重量。
            为什么没把它们收进溢出菜单：真机 E2E（`scripts/e2e/smoke.mjs` step8 / step25）
            直接点 `providers-add-advanced-open`，收进菜单等于把这个 testid 从 DOM 里拿掉。
          */
          <Box sx={{ display: "flex", gap: 2, flexWrap: "wrap" }}>
            <Button variant="text" onClick={refresh}>
              {t("providers.refresh")}
            </Button>
            {/*
              两个入口的按钮文案：两步向导用它的 title 键（「添加服务商」）、高级添加用
              `providers.add.advanced` —— 并排两个「添加」用户分不出谁是谁。

              **这里没有「添加密钥」按钮了**（硬规则 3）：一个服务商一把密钥，
              密钥的写入口是两步向导第 1 步（`pw-key`）与详情页「密钥」块。
              要两个账号 → 再加一条服务商、取不同名字（两句提示见
              `providers.key.multipleHint`）—— 而不是在这里给同一家挂第二份凭据。
            */}
            <Button
              variant="text"
              data-testid="providers-add-advanced-open"
              onClick={() => setAdvancedAddOpen(true)}
            >
              {t("providers.add.advanced")}
            </Button>
            <Button
              variant="contained"
              data-testid="providers-add-open"
              onClick={openProviderWizard}
            >
              {t("providers.add.title")}
            </Button>
          </Box>
        }
      />

      <ProvidersBody
        rows={rows}
        isLoading={providers.isPending}
        error={providers.error ?? undefined}
        onRetry={refresh}
        credentials={credentialRows}
        credentialsError={credentials.error ?? undefined}
        onRetryCredentials={() => void credentials.refetch()}
        onOpen={(id) => navigate(`/providers/${encodeURIComponent(id)}`)}
        onDelete={(row) =>
          openDelete({
            ids: [row.id],
            label: providerLabel(row),
            batch: false,
            consequences: consequencesOf(row),
          })
        }
        selectedIds={selectedIds}
        onToggleSelect={toggleSelect}
        onToggleSelectAll={toggleSelectAll}
        onDeleteSelected={() => {
          const chosen = rows.filter((row) => selectedIds.includes(row.id));
          if (chosen.length === 0) return;
          openDelete({
            ids: chosen.map((row) => row.id),
            label: chosen.map((row) => providerLabel(row)).join("、"),
            batch: true,
            // 批量的连带清单 = 每一条各自的清单（都算得出来才有内容；算不出来就是空）
            consequences: chosen.flatMap((row) =>
              consequencesOf(row).map((line) => `${providerLabel(row)}：${line}`),
            ),
          });
        }}
      />

      {/* 删除的**结果区**（`<prefix>-delete-result`）：只写真的发生过的事 */}
      {deleteResult.length === 0 ? null : (
        <Box sx={{ px: 8, pb: 8 }}>
          <Paper variant="outlined" sx={SECTION_SX}>
            <Stack spacing={1} data-testid="providers-delete-result">
              {deleteResult.map((line, index) => (
                <Typography
                  key={`${index}-${line}`}
                  variant="body1"
                  sx={{ overflowWrap: "anywhere" }}
                >
                  {line}
                </Typography>
              ))}
            </Stack>
          </Paper>
        </Box>
      )}

      {/*
        删除确认：**先确认再发命令**；被引用时后端原文原样显示、
        此时才出现级联按钮；批量一律 `force=false`，所以批量框里没有级联按钮。
      */}
      <ConfirmDelete
        open={deleteTarget !== undefined}
        testIdPrefix="providers"
        title={t("delete.title.provider")}
        targetLabel={deleteTarget?.label ?? ""}
        consequences={deleteTarget?.consequences}
        cascadeAvailable={deleteTarget !== undefined && !deleteTarget.batch}
        pending={removeProvider.isPending || batchRunning}
        errorText={deleteError}
        onConfirm={() => void confirmDelete(false)}
        onConfirmCascade={() => void confirmDelete(true)}
        onCancel={cancelDelete}
      />

      {/*
        两步向导：`providers-add-open` 进的就是它 ——
        第 1 步服务商信息（含落库）、第 2 步添加模型。
      */}
      <ProviderTwoStepWizard
        open={providerAddOpen}
        existing={{ ids: providerIds, names: providerNames }}
        onClose={closeProviderWizard}
      />
      {/*
        原来的**单步**对话框**原样保留**（字段 tid 一个没变），改挂到「高级添加」入口：
        它是真控件、真能提交 —— 老手要的"一次填完 id / 鉴权方式 / 头名 / 值前缀"仍然可达。
        名称重复在这里也要挡住（硬规则 3：名字是用户区分"同一家的不同账号"的抓手）。
      */}
      <ProviderAddDialog
        open={advancedAddOpen}
        existing={{ ids: providerIds, names: providerNames }}
        onClose={() => setAdvancedAddOpen(false)}
      />
    </>
  );
}
