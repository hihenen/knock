import { describe, expect, it } from "bun:test";
import { setupMergeToggle } from "../src/merge-toggle";

class FakeClassList {
  readonly names = new Set<string>(["hidden"]);

  add(name: string) {
    this.names.add(name);
  }

  remove(name: string) {
    this.names.delete(name);
  }
}

class FakeWrap {
  readonly classList = new FakeClassList();
  title = "";
}

class FakeToggle {
  checked = false;
  disabled = false;
  listener?: () => void | Promise<void>;

  addEventListener(_type: "change", listener: () => void | Promise<void>) {
    this.listener = listener;
  }

  async change() {
    await this.listener?.();
  }
}

describe("merge approval header toggle", () => {
  const expiresAt = 1_800_000_000;
  const localTime = (value: number) => {
    const date = new Date(value * 1000);
    return `${String(date.getHours()).padStart(2, "0")}:${String(date.getMinutes()).padStart(2, "0")}`;
  };

  it("stays hidden when the CLI flag is absent", () => {
    const wrap = new FakeWrap();
    const toggle = new FakeToggle();

    setupMergeToggle(
      { gate: true, configAutoApproveMerge: true },
      wrap,
      toggle,
      async () => {},
    );

    expect(wrap.classList.names.has("hidden")).toBe(true);
    expect(toggle.listener).toBeUndefined();
  });

  it("stays hidden on a non-gate annotation even if flagged", () => {
    const wrap = new FakeWrap();
    const toggle = new FakeToggle();

    setupMergeToggle(
      { gate: false, mergeToggle: true, configAutoApproveMerge: true },
      wrap,
      toggle,
      async () => {},
    );

    expect(wrap.classList.names.has("hidden")).toBe(true);
    expect(toggle.listener).toBeUndefined();
  });

  it("shows a flagged gate and reflects the saved boolean", () => {
    const wrap = new FakeWrap();
    const toggle = new FakeToggle();

    setupMergeToggle(
      {
        gate: true,
        mergeToggle: true,
        configAutoApproveMerge: true,
        configAutoApproveMergeExpiresAt: expiresAt,
      },
      wrap,
      toggle,
      async () => {},
    );

    expect(wrap.classList.names.has("hidden")).toBe(false);
    expect(wrap.title).toBe(
      `자동 승인은 ${localTime(expiresAt)} 까지 유지됩니다. 다른 위험 명령이 섞이면 창이 뜹니다.`,
    );
    expect(toggle.checked).toBe(true);
  });

  it("explains that enabling the toggle lasts four hours", () => {
    const wrap = new FakeWrap();
    const toggle = new FakeToggle();

    setupMergeToggle(
      { gate: true, mergeToggle: true, configAutoApproveMerge: false },
      wrap,
      toggle,
      async () => {},
    );

    expect(wrap.title).toBe(
      "켜면 4시간 동안 유지됩니다. 다른 위험 명령이 섞이면 창이 뜹니다.",
    );
  });

  it("only saves the toggle value without resolving the approval", async () => {
    const wrap = new FakeWrap();
    const toggle = new FakeToggle();
    const calls: Array<[string, boolean]> = [];

    setupMergeToggle(
      { gate: true, mergeToggle: true, configAutoApproveMerge: false },
      wrap,
      toggle,
      async (command, args) => {
        calls.push([command, args.enabled]);
      },
    );

    toggle.checked = true;
    await toggle.change();
    toggle.checked = false;
    await toggle.change();

    expect(calls).toEqual([
      ["save_auto_approve_merge", true],
      ["save_auto_approve_merge", false],
    ]);
  });

  it("restores the previous checkbox value when saving fails", async () => {
    const wrap = new FakeWrap();
    const toggle = new FakeToggle();

    setupMergeToggle(
      { gate: true, mergeToggle: true, configAutoApproveMerge: false },
      wrap,
      toggle,
      async () => {
        throw new Error("write failed");
      },
    );

    toggle.checked = true;
    const originalError = console.error;
    console.error = () => {};
    try {
      await toggle.change();
    } finally {
      console.error = originalError;
    }

    expect(toggle.checked).toBe(false);
    expect(toggle.disabled).toBe(false);
  });

  it("updates the header title from the saved expiry returned by Rust", async () => {
    const wrap = new FakeWrap();
    const toggle = new FakeToggle();

    setupMergeToggle(
      { gate: true, mergeToggle: true, configAutoApproveMerge: false },
      wrap,
      toggle,
      async () => ({ enabled: true, expiresAt }),
    );

    toggle.checked = true;
    await toggle.change();

    expect(toggle.checked).toBe(true);
    expect(wrap.title).toBe(
      `자동 승인은 ${localTime(expiresAt)} 까지 유지됩니다. 다른 위험 명령이 섞이면 창이 뜹니다.`,
    );
  });
});
