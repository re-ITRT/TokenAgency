import {
  Box,
  Button,
  Dialog,
  DialogActions,
  DialogContent,
  DialogTitle,
  Stack,
  TextField,
  Typography,
} from "@mui/material";
import { useQueryClient } from "@tanstack/react-query";
import { useEffect, useState, type ReactElement } from "react";

import { api, errorMessage, qk, useCommandMutation, useCommandQuery } from "@/shared/api";
import type { PriceEntry } from "@/shared/bindings";
import { Caption } from "@/shared/components";
import { useTranslation } from "@/shared/i18n";
import {
  draftFromEntry,
  hasPrice,
  invalidPriceFields,
  PRICE_FIELD_LABEL_KEY,
  priceEntryFor,
  type PriceField,
  type PricingDraft,
  EMPTY_PRICING_DRAFT,
} from "@/shared/pricing";

/**
 * 给**一个对外模型**定价（`pricing_upsert` / `pricing_delete`）。
 *
 * # 为什么在模型页
 *
 * 用户第 318 轮：*"成本中心页面只做显示，价格配置到模型页面去"*。
 * 这里放它的理由不只是"用户说了"：定价的对象就是**这一行的模型**，
 * 而模型页每一行已经带着 id —— 对话框不需要再问"给哪个模型定价"
 * （旧实现让用户从服务商/凭据下拉里挑，挑出来的东西和"算得出钱"根本不是一回事）。
 *
 * # 模型字段是**只读展示**，不是输入框
 *
 * 它是从被点的那一行传来的**规范模型 id**。让用户手打模型名会制造两类错：
 * 拼错（价格挂到不存在的模型上）、和写别名（网关归一到规范名后反而匹配不上）。
 * 只读展示 = 这两种错都不可能发生。
 *
 * # 「清除定价」也在这里
 *
 * 成本中心改成只做显示之后，界面上就只剩这一条删除路径了。
 * 删除仍然走 `pricing_delete({index})`，所以**必须**沿用成本中心那条"下标安全"纪律：
 * 删之前重读 `pricing_list`，确认那个下标上**仍然是**用户看到的条目，否则拒绝执行
 * （`pricing_list` 的下标会随任何一次增删整体平移，拿旧下标去删会删错人）。
 */
export function PricingDialog({
  open,
  model,
  onClose,
}: {
  open: boolean;
  /** 被定价的**对外模型 id**（规范名）。 */
  model: string;
  onClose: () => void;
}): ReactElement {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const upsert = useCommandMutation("pricing_upsert");
  const remove = useCommandMutation("pricing_delete");
  // 这一份既是"回填当前价"的来源，也是删除时**下标**的来源。
  const list = useCommandQuery("pricing_list", undefined, qk.pricing);

  const [draft, setDraft] = useState<PricingDraft>(EMPTY_PRICING_DRAFT);
  /** 本地预检点名的价格字段（每次提交重算）。 */
  const [issues, setIssues] = useState<readonly PriceField[]>([]);
  /** 三项全空（= 一条费率都没有）时的点名。与 `issues` 分开：它点的是"整条"。 */
  const [noPrice, setNoPrice] = useState(false);
  /** 删除前的确认（用户点了一次「清除定价」之后才显示）。 */
  const [confirmClear, setConfirmClear] = useState(false);
  /**
   * 点「清除定价」那一刻**锁定**的条目（下标 + 那一格的内容）。
   *
   * 为什么不让确认框继续读实时的 `existing`：确认框一出现，列表就可能被别处改掉
   * （本页自己的重读、别的窗口增删）。若那时实时 `existing` 变成"找不到"，
   * 整个确认框会连同**错误提示一起**消失 —— 用户点了"确认"，什么都没发生，
   * 也没有一句话告诉他为什么。锁定之后：要删的是哪一格在**点的那一刻**就定了，
   * 冲突由 `checkIndexStillOurs` 拒绝并显示，确认框原地留着让用户看到原因。
   */
  const [clearTarget, setClearTarget] = useState<{ entry: PriceEntry; index: number } | undefined>(
    undefined,
  );
  const [clearError, setClearError] = useState<unknown>(null);

  /** 本模型**现有**的条目（没有就是"还没定价"）。 */
  const existing = findEntryForModel(list.data, model);
  const existingIndex = existing?.index ?? null;

  /** 打开时回填：把当前价显示出来，而不是让用户重新打一遍。 */
  useEffect(() => {
    if (!open) return;
    setDraft(draftFromEntry(model, existing?.entry));
    setIssues([]);
    setNoPrice(false);
    setConfirmClear(false);
    setClearTarget(undefined);
    setClearError(null);
    upsert.reset();
    remove.reset();
    // `existing` 只用于"打开那一刻"的回填：之后用户的输入不该被查询刷新覆盖。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, model]);

  const set = <K extends keyof PricingDraft>(key: K, value: PricingDraft[K]): void =>
    setDraft((previous) => ({ ...previous, [key]: value }));

  const close = (): void => {
    setDraft(EMPTY_PRICING_DRAFT);
    setIssues([]);
    setNoPrice(false);
    setConfirmClear(false);
    setClearTarget(undefined);
    setClearError(null);
    onClose();
  };

  const submit = (): void => {
    const found = invalidPriceFields(draft);
    setIssues(found);
    setNoPrice(false);
    // 本地能判定的错就不该拿去让后端兜，更不该让一个注定被拒的载荷落到配置真源上。
    if (found.length > 0) return;
    // 三项全空 = **一条费率都没有**，不能提交：`ResolvedPrice::is_priced()` 只看 `source`，
    // 而我们会写 `source: "manual"` ⇒ 那个模型会从"没定价、花费算不出来"变成
    // "有定价、花费 0"，也就是**把「没定价」写成「免费」**。宁可不发。
    if (!hasPrice(draft)) {
      setNoPrice(true);
      return;
    }
    upsert.mutate(
      { entry: priceEntryFor(draft) },
      {
        onSuccess: () => {
          // 定价表变了 → 未定价清单与花费视图跟着变（同一失效范围），然后关掉。
          void queryClient.invalidateQueries({ queryKey: qk.pricing });
          void queryClient.invalidateQueries({ queryKey: qk.unpricedModels });
          void queryClient.invalidateQueries({ queryKey: qk.dashboardPrefix });
          close();
        },
      },
    );
  };

  const clear = async (): Promise<void> => {
    // 删的是**点「清除定价」那一刻**锁定的那一格，不是"现在这一格"
    // （列表可能已经变了；那正是 `checkIndexStillOurs` 要拦的情况）。
    const target = clearTarget ?? existing;
    if (target === undefined) return;
    setClearError(null);
    try {
      // **下标安全**（[docs/21 §1.1 第 5 条]）：删之前重读一次，确认
      //   那个下标上**仍然是**我们看到的这一条。任何增删都会让下标整体平移，
      //   拿旧下标去删会**删错人** —— 这种错是静默的（界面只是少了另一条价）。
      const fresh = await api.pricing.list();
      const check = checkIndexStillOurs(fresh, target.index, target.entry);
      if (check !== "same") {
        setClearError(
          t(check === "missing" ? "cost.pricing.clear.staleMissing" : "cost.pricing.clear.staleChanged"),
        );
        // 刷新列表，但**不收起确认框**：让用户在原地看到"为什么没删成"，
        // 并且下拉表里现在是什么也看得见。
        void list.refetch();
        return;
      }
      await remove.mutateAsync({ index: target.index });
      void queryClient.invalidateQueries({ queryKey: qk.pricing });
      void queryClient.invalidateQueries({ queryKey: qk.unpricedModels });
      void queryClient.invalidateQueries({ queryKey: qk.dashboardPrefix });
      close();
    } catch (err) {
      // 后端的原文原样显示（TA 码 + 字段路径），本地预检不吞掉它。
      setClearError(err);
    }
  };

  const failure = upsert.error;

  return (
    <Dialog open={open} onClose={close} fullWidth maxWidth="sm">
      <DialogTitle>{t("models.price.title")}</DialogTitle>
      <DialogContent>
        <Stack spacing={2} sx={{ pt: 1 }}>
          {/* 模型只读展示：定价的对象就是这一行的模型，用户不需要（也不应该）再填一次 */}
          <Stack data-testid="models-price-model">
            <Caption>{t("models.price.modelLabel")}</Caption>
            <Typography variant="body1" sx={{ fontFamily: "monospace", overflowWrap: "anywhere" }}>
              {model}
            </Typography>
          </Stack>

          <Caption>{t("cost.pricing.add.unit")}</Caption>
          <Stack direction="row" spacing={2}>
            {(["priceInput", "priceOutput", "priceCached"] as const).map((field) => (
              <TextField
                key={field}
                label={t(PRICE_FIELD_LABEL_KEY[field])}
                value={draft[field]}
                onChange={(event) => set(field, event.target.value)}
                size="small"
                fullWidth
                error={issues.includes(field)}
                slotProps={{
                  // `…-issue-<field>` 的后缀与 `PRICE_FIELDS` 同名，真机 e2e 按它定位
                  htmlInput: { "data-testid": `models-price-${ISSUE_FIELD[field]}`, inputMode: "decimal" },
                }}
              />
            ))}
          </Stack>

          {/* 本地预检的错：逐个点名字段（文案用既有的「必须是数字（可留空）」） */}
          {issues.map((field) => (
            <Typography
              key={field}
              variant="body1"
              data-testid={`models-price-issue-${ISSUE_FIELD[field]}`}
              sx={{ color: "error.main" }}
            >
              {`${t(PRICE_FIELD_LABEL_KEY[field])}：${t("wizard.invalidNumber")}`}
            </Typography>
          ))}

          {/* 三项全空：点的是"整条目"而不是某个字段 */}
          {noPrice ? (
            <Typography variant="body1" data-testid="models-price-issue-none" sx={{ color: "error.main" }}>
              {t("cost.pricing.add.needOne")}
            </Typography>
          ) : null}

          {/* 后端的错原样显示 */}
          {failure !== null && failure !== undefined ? (
            <Typography
              variant="body1"
              data-testid="models-price-error"
              sx={{ color: "error.main", overflowWrap: "anywhere" }}
            >
              {errorMessage(failure)}
            </Typography>
          ) : null}

          {/* 确认框的可见性由**用户点过没点过**决定，而不是"现在还有没有这条价"：
              否则列表一变，用户点了确认却什么都没发生，连原因都看不到。
              要删的条目在下面 `clear()` 里用点那一刻锁下的 `clearTarget`。 */}
          {existingIndex !== null || confirmClear ? (
            confirmClear ? (
              <Stack spacing={1} data-testid="models-price-clear-confirm">
                <Caption>{t("cost.pricing.clear.confirm")}</Caption>
                {clearError !== null && clearError !== undefined ? (
                  <Typography
                    variant="body1"
                    data-testid="models-price-clear-error"
                    sx={{ color: "error.main", overflowWrap: "anywhere" }}
                  >
                    {typeof clearError === "string" ? clearError : errorMessage(clearError)}
                  </Typography>
                ) : null}
                <Stack direction="row" spacing={1}>
                  <Button
                    size="small"
                    variant="text"
                    data-testid="models-price-clear-cancel"
                    onClick={() => {
                      setConfirmClear(false);
                      setClearTarget(undefined);
                      setClearError(null);
                    }}
                  >
                    {t("common.cancel")}
                  </Button>
                  <Button
                    size="small"
                    color="error"
                    data-testid="models-price-clear-execute"
                    disabled={remove.isPending}
                    onClick={() => void clear()}
                  >
                    {t("common.confirm")}
                  </Button>
                </Stack>
              </Stack>
            ) : (
              <Box>
                <Button
                  size="small"
                  color="error"
                  variant="text"
                  data-testid="models-price-clear"
                  onClick={() => {
                    // 锁定"要删哪一格"：此刻的下标与内容一起冻结
                    setClearTarget(existing);
                    setConfirmClear(true);
                  }}
                >
                  {t("cost.pricing.clear.action")}
                </Button>
              </Box>
            )
          ) : null}
        </Stack>
      </DialogContent>
      <DialogActions>
        <Button data-testid="models-price-cancel" onClick={close}>
          {t("common.cancel")}
        </Button>
        <Button
          variant="contained"
          data-testid="models-price-submit"
          disabled={upsert.isPending}
          onClick={submit}
        >
          {t("common.save")}
        </Button>
      </DialogActions>
    </Dialog>
  );
}

/** 价格字段 -> testid 后缀（与 `PRICE_FIELDS` 同名，只是去掉 `price` 前缀）。 */
const ISSUE_FIELD: Record<PriceField, string> = {
  priceInput: "input",
  priceOutput: "output",
  priceCached: "cached",
};

/** `pricing_list` 里属于这个模型的那一条（找不到返回 `undefined`）。 */
export function findEntryForModel(
  raw: unknown,
  model: string,
): { entry: PriceEntry; index: number } | undefined {
  if (!Array.isArray(raw)) return undefined;
  for (let i = 0; i < raw.length; i += 1) {
    const row = raw[i] as { match?: { model?: unknown } } | null;
    const got = row?.match?.model;
    if (typeof got === "string" && got === model) {
      return { entry: raw[i] as PriceEntry, index: i };
    }
  }
  return undefined;
}

/**
 * 删除前的下标复查：那个下标上**还是不是**我们当初看到的那一条。
 *
 * 与成本中心 `checkPricingIndex` 同一条纪律，但判据更贴这次的目的：
 * 这里比的是**"模型对得上"**（`match.model` 相同），而不是整条 JSON 全等 ——
 * 用户可能在别处把这个价改过（同一个模型改价 = 同一条的价格变了），
 * 那时**下标仍然是对的**，拒删反而让用户删不掉。
 * 真正要拒绝的是"这条已经不是那个模型了"（说明列表被增删平移过）。
 */
export function checkIndexStillOurs(
  fresh: unknown,
  index: number,
  expected: PriceEntry | undefined,
): "same" | "missing" | "changed" {
  if (!Array.isArray(fresh) || index < 0 || index >= fresh.length) return "missing";
  const want = expected?.match?.model;
  const got = (fresh[index] as { match?: { model?: unknown } } | null)?.match?.model;
  if (want !== undefined && want !== null && want !== "") {
    return got === want ? "same" : "changed";
  }
  // 没有可比对的模型（理论上不该发生）：退回整条全等，宁可拒删也不删错
  return JSON.stringify(fresh[index]) === JSON.stringify(expected) ? "same" : "changed";
}
