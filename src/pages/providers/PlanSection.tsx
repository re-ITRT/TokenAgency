/**
 * 套餐（用量 / 余额）—— **一个服务商最多一个**。
 *
 * 流程照参考项目「添加套餐」的两步：**先选模板，再填参数**。
 * 数据形状见 `ta-core` 的 `plan` 模块；这里只做展示与调用，**不算任何额度**。
 */
import {
  Box,
  Button,
  Dialog,
  DialogActions,
  DialogContent,
  DialogTitle,
  MenuItem,
  Stack,
  TextField,
  Typography,
} from "@mui/material";
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";

import type { Plan, PlanSnapshot, ProviderId } from "../../shared/bindings";
import { provider as providerApi } from "../../shared/api/commands";

/** 与 `ta-core` 的 `PLAN_TEMPLATES` 一致（只做中转站能查的三个）。 */
const TEMPLATES = [
  { id: "newapi", name: "NewAPI / OneAPI", needsBaseUrl: true },
  { id: "sub2api", name: "Sub2API", needsBaseUrl: true },
  { id: "deepseek", name: "DeepSeek", needsBaseUrl: false },
] as const;

/** 一行额度摘要：窗口型给百分比，余额型给金额。**缺哪种就少显示哪块**。 */
function quotaLines(snap: PlanSnapshot | null): string[] {
  const q = snap?.quota;
  if (!q) return [];
  if (q.kind === "windows") {
    return q.windows.map((w) => `${w.label}：${w.used_percent.toFixed(1)}%`);
  }
  if (q.kind === "fixed") {
    // 不把 `t` 传进这个函数再调用：react-i18next 的 `t` 是重载类型，
    // 作为参数「间接调用」会触发 TypeScript 编译器内部崩溃（Debug Failure）。
    return [
      `${q.used.toFixed(2)} / ${q.total.toFixed(2)} ${q.unit}`,
      `${q.used_percent.toFixed(1)}%`,
    ];
  }
  return [`${q.amount.toFixed(2)} ${q.currency}`];
}

export function PlanSection({ providers }: { providers: Array<{ id: string; name: string }> }) {
  const { t } = useTranslation();
  const [selected, setSelected] = useState<string>(providers[0]?.id ?? "");
  const [plan, setPlan] = useState<Plan | null>(null);
  const [snap, setSnap] = useState<PlanSnapshot | null>(null);
  const [open, setOpen] = useState(false);
  const [step, setStep] = useState<1 | 2>(1);
  const [template, setTemplate] = useState<string>(TEMPLATES[0].id);
  const [threshold, setThreshold] = useState("10");
  const [baseUrl, setBaseUrl] = useState("");
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    if (!selected) return;
    let alive = true;
    providerApi.plan
      .get(selected as ProviderId)
      .then((r) => {
        if (!alive) return;
        setPlan(r.plan);
        setSnap(null);
      })
      .catch(() => {
        if (alive) setPlan(null);
      });
    return () => {
      alive = false;
    };
  }, [selected]);

  if (providers.length === 0) return null;

  const needsBaseUrl = TEMPLATES.find((x) => x.id === template)?.needsBaseUrl ?? false;

  async function refresh() {
    if (!selected) return;
    setBusy(true);
    setError("");
    try {
      setSnap(await providerApi.plan.quota(selected as ProviderId));
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }

  async function save() {
    if (!selected) return;
    setBusy(true);
    setError("");
    try {
      const next: Plan = {
        template,
        enabled: true,
        threshold: Number(threshold) || 0,
        ...(needsBaseUrl && baseUrl.trim() ? { base_url: baseUrl.trim() } : {}),
      };
      await providerApi.plan.set(selected as ProviderId, next);
      setPlan(next);
      setOpen(false);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }

  async function remove() {
    if (!selected) return;
    setBusy(true);
    try {
      await providerApi.plan.clear(selected as ProviderId);
      setPlan(null);
      setSnap(null);
    } finally {
      setBusy(false);
    }
  }

  function startAdd() {
    setStep(1);
    setTemplate(TEMPLATES[0].id);
    setThreshold(String(plan?.threshold ?? 10));
    setBaseUrl(plan?.base_url ?? "");
    setError("");
    setOpen(true);
  }

  return (
    <Stack spacing={2} data-testid="providers-plan-section">
      <Stack direction="row" spacing={2} alignItems="center" sx={{ flexWrap: "wrap" }}>
        <Typography variant="h4">{t("providers.plan.title")}</Typography>
        <TextField
          select
          size="small"
          label={t("providers.plan.pickProvider")}
          value={selected}
          onChange={(e) => setSelected(e.target.value)}
          sx={{ minWidth: 200 }}
        >
          {providers.map((p) => (
            <MenuItem key={p.id} value={p.id}>
              {p.name}
            </MenuItem>
          ))}
        </TextField>
        <Button variant="outlined" onClick={startAdd}>
          {plan ? t("providers.plan.edit") : t("providers.plan.configure")}
        </Button>
        {plan ? (
          <>
            <Button variant="outlined" disabled={busy} onClick={refresh}>
              {t("providers.plan.refresh")}
            </Button>
            <Button variant="outlined" disabled={busy} onClick={remove}>
              {t("providers.plan.remove")}
            </Button>
          </>
        ) : null}
      </Stack>

      {plan ? (
        <Box data-testid="providers-plan-quota">
          {snap?.ok ? (
            quotaLines(snap).map((line) => (
              <Typography key={line} variant="body2">
                {line}
              </Typography>
            ))
          ) : snap ? (
            <Typography variant="body2">
              {t("providers.plan.failed", { message: snap.error ?? "" })}
            </Typography>
          ) : (
            <Typography variant="body2">{t("providers.plan.hint")}</Typography>
          )}
        </Box>
      ) : (
        <Typography variant="body2">{t("providers.plan.none")}</Typography>
      )}

      {error ? (
        <Typography variant="body2">{t("providers.plan.failed", { message: error })}</Typography>
      ) : null}

      <Dialog open={open} onClose={() => setOpen(false)} fullWidth maxWidth="xs">
        <DialogTitle>
          {step === 1 ? t("providers.plan.step1") : t("providers.plan.step2")}
        </DialogTitle>
        <DialogContent>
          {step === 1 ? (
            <Stack spacing={1}>
              {TEMPLATES.map((x) => (
                <Button
                  key={x.id}
                  variant={template === x.id ? "contained" : "outlined"}
                  onClick={() => setTemplate(x.id)}
                >
                  {x.name}
                </Button>
              ))}
            </Stack>
          ) : (
            <Stack spacing={2} sx={{ pt: 1 }}>
              {needsBaseUrl ? (
                <TextField
                  label={t("providers.plan.baseUrl")}
                  value={baseUrl}
                  onChange={(e) => setBaseUrl(e.target.value)}
                  placeholder="https://relay.example.com"
                />
              ) : null}
              <TextField
                label={t("providers.plan.threshold")}
                value={threshold}
                onChange={(e) => setThreshold(e.target.value)}
                type="number"
              />
            </Stack>
          )}
        </DialogContent>
        <DialogActions>
          <Button onClick={() => setOpen(false)}>{t("providers.plan.cancel")}</Button>
          {step === 1 ? (
            <Button variant="contained" onClick={() => setStep(2)}>
              {t("providers.plan.next")}
            </Button>
          ) : (
            <Button variant="contained" disabled={busy} onClick={save}>
              {t("providers.plan.save")}
            </Button>
          )}
        </DialogActions>
      </Dialog>
    </Stack>
  );
}
