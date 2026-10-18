/**
 * 主界面的套餐额度总览。
 *
 * 只显示**配了套餐**的服务商；一个都没配就整块不出现（不设门槛，也不画空壳）。
 * 额度来源见 `ta-core` 的 `plan` 模块；这里只读，不算。
 */
import { Paper, Stack, Typography } from "@mui/material";
import { useEffect, useState } from "react";

import { useTranslation } from "@/shared/i18n";
import { provider as providerApi, qk, useCommandQuery } from "@/shared/api";
import type { Plan, PlanSnapshot } from "@/shared/bindings";

type Row = { id: string; name: string; plan: Plan; snap: PlanSnapshot | null };

/** 一行额度摘要：窗口型给百分比，固定额度给 `已用/总额 单位`，余额型给金额。 */
function quotaLines(snap: PlanSnapshot | null): string[] {
  const q = snap?.quota;
  if (!q) return [];
  if (q.kind === "windows") return q.windows.map((w) => `${w.label}：${w.used_percent.toFixed(1)}%`);
  if (q.kind === "fixed") {
    return [`${q.used.toFixed(2)} / ${q.total.toFixed(2)} ${q.unit}`, `${q.used_percent.toFixed(1)}%`];
  }
  return [`${q.amount.toFixed(2)} ${q.currency}`];
}

export function PlanOverview() {
  const { t } = useTranslation();
  const providers = useCommandQuery("provider_list", undefined, qk.providers);
  const [rows, setRows] = useState<Row[]>([]);
  const key = (providers.data ?? []).map((p) => String(p.id)).join(",");

  useEffect(() => {
    let alive = true;
    const list = providers.data ?? [];
    void (async () => {
      const out: Row[] = [];
      for (const p of list) {
        try {
          const r = await providerApi.plan.get(p.id);
          if (!r.plan) continue;
          const snap = await providerApi.plan.quota(p.id).catch(() => null);
          out.push({ id: String(p.id), name: p.name, plan: r.plan, snap });
        } catch {
          // 单个服务商查不了不影响其它 —— 但也不假装它有额度
        }
      }
      if (alive) setRows(out);
    })();
    return () => {
      alive = false;
    };
  }, [key, providers.data]);

  if (rows.length === 0) return null;

  return (
    <Paper sx={{ p: 2 }} data-testid="dashboard-plan-overview">
      <Typography variant="h4" sx={{ mb: 1 }}>
        {t("dashboard.plan.title")}
      </Typography>
      <Stack spacing={1}>
        {rows.map((r) => (
          <Stack key={r.id} direction="row" spacing={2} alignItems="baseline" sx={{ flexWrap: "wrap" }}>
            <Typography variant="body2">{r.name}</Typography>
            {r.snap?.ok ? (
              quotaLines(r.snap).map((line) => (
                <Typography key={line} variant="body2">
                  {line}
                </Typography>
              ))
            ) : (
              <Typography variant="body2">
                {t("providers.plan.failed", { message: r.snap?.error ?? "" })}
              </Typography>
            )}
          </Stack>
        ))}
      </Stack>
    </Paper>
  );
}
