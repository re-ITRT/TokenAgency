import { invoke } from "@tauri-apps/api/core";

/**
 * 与 Rust 侧 `IpcError`一一对应的错误形状。
 * 所有 IPC 命令都返回 `Result<T, IpcError>`。
 */
export type IpcError = {
  code: string; // 错误码，见 （TA-xxxx）
  message: string;
  details?: unknown;
};

/** 领域化错误：页面可以直接按 `code` 分支，不用解析 message 字符串。 */
export class TokenAgencyError extends Error {
  readonly code: string;
  readonly details?: unknown;

  constructor(e: IpcError) {
    super(e.message);
    this.name = "TokenAgencyError";
    this.code = e.code;
    this.details = e.details;
  }

  /** 是否是"用户可自行修复"的错误（配置错 / 被引用 / 明文密钥） */
  get isFixable(): boolean {
    return this.code.startsWith("TA-5");
  }
}

function toIpcError(raw: unknown): IpcError {
  if (typeof raw === "object" && raw !== null && "code" in raw && "message" in raw) {
    const o = raw as Record<string, unknown>;
    return { code: String(o.code), message: String(o.message), details: o.details };
  }
  return { code: "TA-9001", message: typeof raw === "string" ? raw : String(raw) };
}

/** 是否运行在 Tauri 宿主里（浏览器里开发时为 false，会走 mock） */
export function isTauri(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

// ---------------------------------------------------------------- mock（仅开发用）

type MockHandler = (args?: Record<string, unknown>) => unknown;
const mocks = new Map<string, MockHandler>();

/**
 * 注册一个命令的 mock。**仅在 `import.meta.env.DEV` 且不在 Tauri 宿主时生效。**
 * 目的：F1–F4 可以在浏览器里开发页面，不必等后端全部就绪。
 */
export function registerMock(cmd: string, handler: MockHandler): void {
  mocks.set(cmd, handler);
}

// ---------------------------------------------------------------- 调用入口

/**
 * 唯一 IPC 调用入口。**页面禁止直接 import `@tauri-apps/api`。**
 *
 * - 统一把后端 `IpcError` 转成 `TokenAgencyError`
 * - 非 Tauri 环境下走 mock（没有注册 mock 则抛错，避免静默拿到 undefined）
 * - 命令名必须与 `src/shared/bindings.ts`（tauri-specta 生成）里的一致
 */
export async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (!isTauri()) {
    const h = mocks.get(cmd);
    if (!h) {
      throw new TokenAgencyError({
        code: "TA-9001",
        message: `非 Tauri 环境且未注册 mock：${cmd}。页面开发时请先 registerMock("${cmd}", ...)。`,
      });
    }
    return (await h(args)) as T;
  }
  try {
    return await invoke<T>(cmd, args);
  } catch (raw) {
    throw new TokenAgencyError(toIpcError(raw));
  }
}

/** 静默版：失败返回 fallback，用于"锦上添花"的可选数据（如托盘延迟）。 */
export async function callOr<T>(cmd: string, fallback: T, args?: Record<string, unknown>): Promise<T> {
  try {
    return await call<T>(cmd, args);
  } catch {
    return fallback;
  }
}
