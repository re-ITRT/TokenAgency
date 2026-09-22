// @vitest-environment jsdom
/**
 * **写命令成功后，`qk.config` 必须被重新拉取** —— 这是数据安全规则，不是"顺手刷新"。
 *
 * 为什么必须有一条测试守着它（真机 E2E 抓到过的用户可见数据丢失）：
 * `config_get` 返回**整份** `ConfigDoc`，而 `config_apply` 是**全量覆盖**
 * （Rust `config_store.rs::write_rows` 先 `DELETE FROM config` 再插入；
 * `src-tauri/src/commands/config.rs` 的 `config_apply` 直接 `save` + `set_doc`）。
 * 所以"前端手里的那份 `qk.config` 旧不旧"直接决定**用户建好的东西会不会被下一次写删掉**：
 *
 *   1. 用户在 `/network` 建一条线路（`lane_upsert`）；
 *   2. 3 秒内打开设置页 —— `main.tsx` 的 `staleTime: 5_000` 把那次 `config_get` 当成 fresh，
 *      **不重取**（而各写命令成功后并没有让 `qk.config` 失效，见下）；
 *   3. 用户改一个字段点「校验并应用」→ 提交的是**建线路之前**的那份 doc → 线路被静默抹掉。
 *
 * 设计上本该兜底的那条通道（`config:changed` 事件 → `EVENT_INVALIDATIONS["config:changed"]`）
 * 目前**没有生产端生产者**（`TaEvent::ConfigChanged` 只在测试里被构造），
 * 所以这条测试断言的是**唯一真正生效的那道保险**：`useCommandMutation` 的集中失效。
 *
 * 这条测试**不**去断言"某个页面调了 invalidateQueries"（那会把实现细节焊死），
 * 它断言的是**可观察后果**：写成功之后，`config_get` 被重新问了一次。
 */

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { registerMock } from "@/shared/api/client";
import { qk } from "@/shared/api/keys";
import { useCommandMutation, useCommandQuery } from "@/shared/api/queries";

/** `config_get` 被问了几次（断言"有没有重新拉"，不看内容）。 */
let configGetCalls = 0;

/**
 * 一个**只读取 `lanes.length` 的最小文档**：第 2 次及以后多出一条 lane。
 * 这样"前端拿到的是不是最新配置"这件事在界面上直接可见 —— 与真实缺陷同一形状。
 */
function docWithLanes(count: number): unknown {
  return {
    version: 1,
    lanes: Array.from({ length: count }, (_, i) => ({ id: `lane-${i + 1}` })),
  };
}

function Probe() {
  const config = useCommandQuery("config_get", undefined, qk.config);
  // `lane_delete` 入参最简单（`{ id }`）；本测试只关心"写命令成功之后发生了什么"。
  const write = useCommandMutation("lane_delete");
  return (
    <div>
      <span data-testid="lane-count">
        {config.data === undefined ? "pending" : String(config.data.lanes.length)}
      </span>
      <button type="button" onClick={() => write.mutate({ id: "lane-1" })}>
        write
      </button>
    </div>
  );
}

function renderProbe() {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: 0, staleTime: 5_000 } },
  });
  return render(
    <QueryClientProvider client={queryClient}>
      <Probe />
    </QueryClientProvider>,
  );
}

beforeEach(() => {
  configGetCalls = 0;
  // 与真实缺陷同形：第二次（及以后）读到的配置里**多了**一条 lane。
  registerMock("config_get", () => {
    configGetCalls += 1;
    return docWithLanes(configGetCalls >= 2 ? 1 : 0);
  });
  registerMock("lane_delete", () => undefined);
});

afterEach(() => {
  cleanup();
});

describe("[数据安全] 写命令成功后 qk.config 必须重新拉取", () => {
  it("写成功后 config_get 被重新问、界面看到的是最新配置（不再是旧 doc）", async () => {
    renderProbe();
    await screen.findByText("0", { exact: true });
    expect(configGetCalls).toBe(1);

    fireEvent.click(screen.getByRole("button", { name: "write" }));

    // 红先绿后：没有集中失效时，这里永远停在 1 次 / 界面上永远是 "0"。
    await waitFor(() => expect(configGetCalls).toBeGreaterThanOrEqual(2));
    await screen.findByText("1", { exact: true });
    expect(screen.getByTestId("lane-count").textContent).toBe("1");
  });

  it("写失败时不重新拉配置（失败不该被当成配置变了）", async () => {
    registerMock("lane_delete", () => {
      throw new Error("boom");
    });
    renderProbe();
    await screen.findByText("0", { exact: true });
    expect(configGetCalls).toBe(1);

    fireEvent.click(screen.getByRole("button", { name: "write" }));

    // 等一小会儿：只要是"失败也失效"，就会变成 2（这条用例会红）。
    await new Promise((resolve) => setTimeout(resolve, 150));
    expect(configGetCalls).toBe(1);
  });
});
