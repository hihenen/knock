interface MergeTogglePayload {
  gate: boolean;
  mergeToggle?: boolean;
  configAutoApproveMerge?: boolean;
  configAutoApproveMergeExpiresAt?: number | null;
}

export interface AutoApproveMergeStatus {
  enabled: boolean;
  expiresAt: number | null;
}

interface ToggleWrap {
  classList: {
    add(name: string): void;
    remove(name: string): void;
  };
  title: string;
}

interface ToggleInput {
  checked: boolean;
  disabled: boolean;
  addEventListener(type: "change", listener: () => void | Promise<void>): void;
}

type Invoke = (command: string, args: { enabled: boolean }) => Promise<unknown>;

const RISK_NOTE = "다른 위험 명령이 섞이면 창이 뜹니다.";

export function autoApproveMergeStatusText(
  enabled: boolean,
  expiresAt?: number | null,
): string {
  if (!enabled) return "켜면 4시간 동안 유지됩니다.";
  if (expiresAt == null) return "켜면 4시간 동안 유지됩니다.";
  const date = new Date(expiresAt * 1000);
  const until = `${String(date.getHours()).padStart(2, "0")}:${String(date.getMinutes()).padStart(2, "0")}`;
  return `자동 승인은 ${until} 까지 유지됩니다.`;
}

export function autoApproveMergeTitle(enabled: boolean, expiresAt?: number | null): string {
  return `${autoApproveMergeStatusText(enabled, expiresAt)} ${RISK_NOTE}`;
}

function readStatus(value: unknown): AutoApproveMergeStatus | null {
  if (typeof value !== "object" || value === null) return null;
  const status = value as Partial<AutoApproveMergeStatus>;
  if (typeof status.enabled !== "boolean") return null;
  if (status.expiresAt !== null && typeof status.expiresAt !== "number") return null;
  return { enabled: status.enabled, expiresAt: status.expiresAt ?? null };
}

export function setupMergeToggle(
  payload: MergeTogglePayload,
  wrap: ToggleWrap,
  toggle: ToggleInput,
  invoke: Invoke,
): void {
  wrap.classList.add("hidden");
  toggle.checked = false;
  toggle.disabled = false;

  if (!payload.gate || payload.mergeToggle !== true) return;

  wrap.classList.remove("hidden");
  wrap.title = autoApproveMergeTitle(
    payload.configAutoApproveMerge === true,
    payload.configAutoApproveMergeExpiresAt,
  );
  toggle.checked = payload.configAutoApproveMerge === true;
  toggle.addEventListener("change", async () => {
    const enabled = toggle.checked;
    toggle.disabled = true;
    try {
      const status = readStatus(await invoke("save_auto_approve_merge", { enabled }));
      if (status) {
        toggle.checked = status.enabled;
        wrap.title = autoApproveMergeTitle(status.enabled, status.expiresAt);
      }
    } catch (error) {
      console.error("auto_approve_merge 설정 저장 실패:", error);
      toggle.checked = !enabled;
    } finally {
      toggle.disabled = false;
    }
  });
}
