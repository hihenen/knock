interface MergeTogglePayload {
  gate: boolean;
  mergeToggle?: boolean;
  configAutoApproveMerge?: boolean;
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

const TITLE = "켜면 이후 머지 승인 창 없이 바로 실행됩니다. 다른 위험 명령이 섞이면 창이 뜹니다.";

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
  wrap.title = TITLE;
  toggle.checked = payload.configAutoApproveMerge === true;
  toggle.addEventListener("change", async () => {
    const enabled = toggle.checked;
    toggle.disabled = true;
    try {
      await invoke("save_auto_approve_merge", { enabled });
    } catch (error) {
      console.error("auto_approve_merge 설정 저장 실패:", error);
      toggle.checked = !enabled;
    } finally {
      toggle.disabled = false;
    }
  });
}
