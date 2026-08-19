import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow, LogicalSize } from "@tauri-apps/api/window";

interface AnnotatePayload {
  mode: "annotate";
  html: string;
  title: string;
  gate: boolean;
  touchId?: boolean;
  configTouchId?: boolean;
  configOpenUrl?: boolean;
  configTts?: boolean;
  configTtsScope?: string;
  configTtsVoice?: string;
  configTtsRepeat?: number;
  actionUrl?: string | null;
  /// 승인 후에도 큐에 남겨 절차를 다시 볼 수 있게 하고, "완료"로 resolve 한다.
  checklist?: boolean;
  /// 이 요청이 이미 승인돼 진행 중인가 (큐에서 다시 열었을 때 채워진다).
  inProgress?: boolean;
}
interface AskOption {
  label: string;
  description?: string;
}
interface AskQuestion {
  header?: string;
  question?: string;
  multiSelect?: boolean;
  options?: AskOption[];
}
interface AskPayload {
  mode: "ask";
  title: string;
  questions: { questions?: AskQuestion[] };
  contextHtml?: string | null;
  configTouchId?: boolean;
  configTts?: boolean;
  configTtsScope?: string;
  configTtsVoice?: string;
  configTtsRepeat?: number;
}
interface SettingsPayload {
  mode: "settings";
  touchId: boolean;
  tts?: boolean;
  ttsStyle?: string;
  ttsScope?: string;
  ttsVoice?: string;
  ttsRepeat?: number;
  ttsPhrase?: string;
  version?: string;
}
type Payload = AnnotatePayload | AskPayload | SettingsPayload;

interface QueueItem {
  id: string;
  kind: "annotate" | "ask";
  title: string;
  source?: {
    project?: string;
    caller?: string;
  };
  createdAt?: number;
  payload: AnnotatePayload | AskPayload;
  /// 승인은 됐지만 아직 완료되지 않은 --checklist 요청.
  inProgress?: boolean;
}
interface QueuePayload {
  mode: "queue";
  items: QueueItem[];
  touchId: boolean;
}

// Where a finished decision goes. Default = single-shot (process exits on submit).
// In daemon mode this is swapped for one that resolves a specific queued request
// and reloads the page to show the next item / the list.
type Sink = {
  annotate: (decision: string, feedback?: string, completed?: boolean) => void;
  // --checklist 승인: resolve 하지 않고 큐에 "진행 중"으로 남긴다.
  // 단일창 모드에는 큐가 없어 undefined — 그 경우 기존처럼 즉시 resolve 한다.
  startAction?: () => void;
  // `grant` = the owner ticked "send as execution approval"; carries a TTL so the
  // next knock permission gate auto-approves once.
  ask: (answers: Record<string, string | string[]>, grant?: boolean) => void;
  dismiss: () => void;
};

let sink: Sink = {
  annotate: (decision, feedback) =>
    invoke("submit", { decision, feedback: feedback ?? null }),
  // Only the boolean intent crosses the boundary; the TTL policy lives in Rust.
  ask: (answers, grant) =>
    invoke("submit_answers", { answers, grant: !!grant }),
  dismiss: () => invoke("dismiss"),
};

const $ = <T extends HTMLElement>(id: string) =>
  document.getElementById(id) as T;
const el = (tag: string, cls?: string, text?: string) => {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text) e.textContent = text;
  return e;
};

// Embedded HTML (diagrams, charts) is author-supplied content shown *inside* the
// approval window. The Rust side already forces `sandbox="allow-scripts"` without
// `allow-same-origin`, so it cannot reach this document. What it still can do is
// draw anything inside its own box -- including a fake approval header. So we
// frame it and label it, making the boundary visible rather than invisible.
function decorateEmbeds(container: HTMLElement) {
  container.querySelectorAll<HTMLIFrameElement>("iframe").forEach((f) => {
    if (f.parentElement?.classList.contains("embed")) return;
    const box = document.createElement("figure");
    box.className = "embed";
    const cap = document.createElement("figcaption");
    cap.textContent = "외부 콘텐츠 — 아래 승인 버튼과 무관";
    f.replaceWith(box);
    box.appendChild(cap);
    box.appendChild(f);
  });
  // Wide tables get their own scroller so cells are not crushed in a narrow window.
  container.querySelectorAll<HTMLTableElement>("table").forEach((t) => {
    if (t.parentElement?.classList.contains("table-scroll")) return;
    const wrap = document.createElement("div");
    wrap.className = "table-scroll";
    t.replaceWith(wrap);
    wrap.appendChild(t);
  });
}

// Open http(s) links from rendered markdown in the real browser, not the webview.
/// The scroll container depends on which view is up: annotate renders into
/// `#content`, ask into `#ask-root`. Pick whichever is visible *and* actually
/// overflows, so a key press on a short body does nothing rather than scrolling
/// some unrelated element.
function scrollableBody(): HTMLElement | null {
  for (const id of ["content", "ask-root", "settings-root"]) {
    const e = document.getElementById(id);
    if (!e || e.classList.contains("hidden")) continue;
    if (e.scrollHeight > e.clientHeight + 4) return e;
  }
  return null;
}

/// One press moves just under a screenful, keeping a couple of lines of overlap
/// so a sentence is never split across two presses.
function scrollBody(dir: "up" | "down") {
  const box = scrollableBody();
  if (!box) return;
  const step = Math.max(120, box.clientHeight * 0.85);
  box.scrollBy({ top: dir === "up" ? -step : step, behavior: "smooth" });
}

function makeLinksExternal(container: HTMLElement) {
  container.querySelectorAll<HTMLAnchorElement>("a[href]").forEach((a) => {
    a.addEventListener("click", (e) => {
      const href = a.getAttribute("href") || "";
      if (
        href.startsWith("http://") ||
        href.startsWith("https://") ||
        href.startsWith("file://")
      ) {
        e.preventDefault();
        invoke("open_url", { url: href }).catch(() => {});
      }
    });
  });
}

// =====================================================================
// update-available check (notify-don't-install; brew upgrade for installs)
// =====================================================================
const REPO = "hihenen/knock";
const BREW_CMD = "brew upgrade hihenen/tap/knock";

function cmpVer(a: string, b: string): number {
  const pa = a.split(".").map((n) => parseInt(n, 10) || 0);
  const pb = b.split(".").map((n) => parseInt(n, 10) || 0);
  for (let i = 0; i < 3; i++) {
    if ((pa[i] || 0) !== (pb[i] || 0)) return (pa[i] || 0) - (pb[i] || 0);
  }
  return 0;
}

async function fetchLatest(): Promise<string | null> {
  const now = Date.now();
  const last = +(localStorage.getItem("knock_update_check") || 0);
  const cached = localStorage.getItem("knock_latest");
  // 24h throttle to respect GitHub's 60 req/hr unauthenticated limit.
  if (last && now - last < 86_400_000 && cached) return cached;
  try {
    const res = await fetch(
      `https://api.github.com/repos/${REPO}/releases/latest`,
    );
    if (!res.ok) return cached;
    const data = await res.json();
    const latest = String(data.tag_name || "").replace(/^v/, "");
    if (latest) {
      localStorage.setItem("knock_update_check", String(now));
      localStorage.setItem("knock_latest", latest);
    }
    return latest || cached;
  } catch {
    return cached; // fail silent on network error
  }
}

async function checkUpdateBanner() {
  let current: string;
  try {
    current = await invoke<string>("app_version");
  } catch {
    return;
  }
  const latest = await fetchLatest();
  if (!latest || cmpVer(latest, current) <= 0) return; // already latest
  if (localStorage.getItem("knock_dismissed_update") === latest) return;

  const banner = $("update-banner");
  $("update-text").textContent = `🆕 knock v${latest} 사용 가능 — ${BREW_CMD}`;
  $("update-copy").addEventListener("click", () => {
    navigator.clipboard?.writeText(BREW_CMD).catch(() => {});
  });
  $("update-notes").addEventListener("click", (e) => {
    e.preventDefault();
    invoke("open_url", {
      url: `https://github.com/${REPO}/releases/latest`,
    }).catch(() => {});
  });
  $("update-dismiss").addEventListener("click", () => {
    localStorage.setItem("knock_dismissed_update", latest);
    banner.classList.add("hidden");
  });
  banner.classList.remove("hidden");
}

let submitted = false;
function once(fn: () => void) {
  if (submitted) return;
  submitted = true;
  fn();
}

// In the daemon's reused window we re-render in place, so the window-level
// keydown handler must be swapped (not stacked) on each view.
let activeKey: ((e: KeyboardEvent) => void) | null = null;
function setKey(h: ((e: KeyboardEvent) => void) | null) {
  if (activeKey) window.removeEventListener("keydown", activeKey);
  activeKey = h;
  if (h) window.addEventListener("keydown", h);
}

type WindowLayout = "compact" | "ask-single" | "large" | "queue" | "settings";
let activeWindowLayout = "";

// Fixed pixel sizes assumed a screen size. On a large display a content-rich gate
// opened into a letterbox; on a small one it could overflow. Size against the
// *available* screen area instead (screen.avail* excludes menu bar and dock, and
// is already in logical px -- the same unit LogicalSize wants).
function screenBox() {
  const w = window.screen?.availWidth || 1440;
  const h = window.screen?.availHeight || 900;
  return { w, h };
}

const SIZE_KEY = "knock_win_size";

/// Sizes the owner set by hand, per layout. Their choice outranks our defaults --
/// they are looking at the content and we are guessing from its length.
function readRememberedSizes(): Record<string, [number, number]> {
  try {
    return JSON.parse(localStorage.getItem(SIZE_KEY) || "{}");
  } catch {
    return {};
  }
}

function rememberSize(layoutKey: string, width: number, height: number) {
  const all = readRememberedSizes();
  all[layoutKey] = [Math.round(width), Math.round(height)];
  try {
    localStorage.setItem(SIZE_KEY, JSON.stringify(all));
  } catch {
    // Non-fatal: we just lose the memory for next time.
  }
}

/// Persist manual resizes for whichever layout is on screen. Debounced so a drag
/// writes once, and only after the window has settled.
let resizeTimer: number | undefined;
function watchManualResize() {
  const win = getCurrentWindow();
  void win.onResized(() => {
    window.clearTimeout(resizeTimer);
    resizeTimer = window.setTimeout(async () => {
      if (!activeWindowLayout) return;
      try {
        const size = await win.innerSize();
        const factor = await win.scaleFactor();
        const logical = size.toLogical(factor);
        rememberSize(activeWindowLayout, logical.width, logical.height);
      } catch {
        // ignore
      }
    }, 400);
  });
}

async function applyWindowLayout(layout: WindowLayout, itemCount = 0) {
  const layoutKey = layout === "queue" ? `${layout}:${Math.min(itemCount, 8)}` : layout;
  if (activeWindowLayout === layoutKey) return;
  activeWindowLayout = layoutKey;

  const { w: availW, h: availH } = screenBox();
  const cap = (width: number, height: number): [number, number] => [
    Math.round(Math.min(width, availW * 0.96)),
    Math.round(Math.min(height, availH * 0.96)),
  ];

  const [width, height] = (() => {
    const remembered = readRememberedSizes()[layoutKey];
    if (remembered) return cap(remembered[0], remembered[1]);
    switch (layout) {
      case "ask-single":
        return cap(Math.min(980, availW * 0.7), availH * 0.72);
      case "queue":
        return cap(
          Math.min(900, availW * 0.65),
          Math.max(540, Math.min(availH * 0.8, 250 + itemCount * 58)),
        );
      case "settings":
        return cap(800, Math.min(820, availH * 0.8));
      // Content-rich gates: nearly the full height of the screen. Reading a long
      // body in a short window means scrolling past the decision itself.
      case "large":
        return cap(Math.min(1320, availW * 0.86), availH * 0.94);
      default:
        return cap(Math.min(1000, availW * 0.72), availH * 0.82);
    }
  })();

  try {
    const win = getCurrentWindow();
    await win.setSize(new LogicalSize(width, height));
    await win.center();
  } catch {
    // Layout is an enhancement; never block a gate if the window API fails.
  }
}

// The daemon reuses one window across many queued requests. The setup
// functions bind click/change/paste listeners onto STATIC elements (opt-approve,
// ask-submit, td-toggle, ...). Those listeners are NOT auto-removed on re-render,
// so without this they STACK: approving the Nth item would fire every prior
// item's approve closure too — opening a browser tab for each stale actionUrl
// (the "multiple tabs / unrelated old link" bug). Clone-replacing the
// listener-bearing containers drops all accumulated listeners; the next setup
// binds fresh ones. getElementById still resolves (clone keeps ids).
function dropStaleListeners() {
  for (const id of [
    "td-toggle-wrap", // td-toggle (header)
    "tts-toggle-wrap", // tts-header-toggle (header)
    "annotate-footer", // opt-approve, opt-cancel, feedback, send + focusin
    "ask-footer", // ask-dismiss, ask-prev, ask-next, ask-submit
    "ask-root", // focusin (children are innerHTML-reset below)
    "settings-root", // touch-id-toggle, report-bug, release-notes
    "settings-footer",
  ]) {
    const e = document.getElementById(id);
    if (e) e.replaceWith(e.cloneNode(true));
  }
}

// Hide every mode section + footer and reset per-view state before rendering
// the next one (daemon window is reused across requests).
function resetView() {
  dropStaleListeners();
  for (const id of [
    "content",
    "ask-root",
    "settings-root",
    "annotate-footer",
    "ask-footer",
    "settings-footer",
  ]) {
    document.getElementById(id)?.classList.add("hidden");
  }
  document.getElementById("td-toggle-wrap")?.classList.add("hidden");
  document.getElementById("tts-toggle-wrap")?.classList.add("hidden");
  const ar = document.getElementById("ask-root");
  if (ar) ar.innerHTML = "";
  submitted = false;
  setKey(null);
}

// =====================================================================
// annotate mode
// =====================================================================
function sendDecision(
  decision: "approved" | "annotated" | "dismissed",
  feedback?: string,
  completed?: boolean,
) {
  once(() => sink.annotate(decision, feedback, completed));
}

// Header 🔊 toggle — shown on every gate (annotate/ask) so the owner can mute
// or unmute the spoken alert right from the window. Persists to config `tts`
// (same key as the tray + settings toggles), so it survives across gates.
function wireTtsHeader(configTts?: boolean) {
  const wrap = $("tts-toggle-wrap");
  const toggle = $<HTMLInputElement>("tts-header-toggle");
  wrap.classList.remove("hidden");
  toggle.checked = configTts ?? false;
  toggle.addEventListener("change", () => {
    invoke("save_tts", { enabled: toggle.checked });
  });
}

function setupAnnotate(p: AnnotatePayload) {
  void applyWindowLayout(p.html.length > 6000 ? "large" : "compact");
  $("badge").textContent = "승인 요청";
  $("title").textContent = p.title;
  $("content").innerHTML = p.html;
  $("content").classList.remove("hidden");
  makeLinksExternal($("content"));
  decorateEmbeds($("content"));
  $("annotate-footer").classList.remove("hidden");
  wireTtsHeader(p.configTts);

  const optApprove = $("opt-approve");
  const optCancel = $("opt-cancel");
  const feedback = $<HTMLTextAreaElement>("feedback");
  const sendBtn = $<HTMLButtonElement>("send");

  if (!p.gate) optApprove.classList.add("hidden");

  // Header Touch ID toggle — reflects the saved config and also applies to this
  // approval. Flipping it persists to config (next critical gates) immediately.
  const tdWrap = $("td-toggle-wrap");
  const tdToggle = $<HTMLInputElement>("td-toggle");
  const explicitTouchId = p.touchId === true;
  const approveLabel = optApprove.querySelector(".ask-opt-label");
  const hasAction = !!p.actionUrl;
  // --checklist 는 승인 시점이 아니라 "완료" 시점에 resolve 한다. 큐에 진행 중으로
  // 남아 있다가 다시 열리면 버튼이 완료로 바뀐다. startAction 이 없는 단일창
  // 모드에는 큐가 없으므로 기존처럼 승인 즉시 resolve 한다.
  const checklistMode = !!p.checklist && !!sink.startAction;
  const resuming = checklistMode && !!p.inProgress;
  const reflectLabel = () => {
    if (!approveLabel) return;
    if (resuming) {
      approveLabel.textContent = "✓ 완료";
      return;
    }
    const base = tdToggle.checked ? "🔒 Touch ID 승인" : "✓ 승인";
    const suffix = checklistMode ? " → 링크 열기 (완료는 나중에)" : " → 링크 열기";
    approveLabel.textContent = hasAction ? `${base}${suffix}` : base;
  };
  if (p.gate) {
    tdWrap.classList.remove("hidden");
    tdToggle.checked = explicitTouchId || (p.configTouchId ?? false);
    tdToggle.disabled = explicitTouchId;
    tdWrap.title = explicitTouchId
      ? "이 요청은 Touch ID 인증이 필수입니다"
      : "이 요청과 이후 요청에 Touch ID 요구";
    reflectLabel();
    if (!explicitTouchId) {
      tdToggle.addEventListener("change", () => {
        invoke("save_touch_id", { enabled: tdToggle.checked });
        reflectLabel();
      });
    }
  }

  // Approve, optionally gated behind Touch ID / Windows Hello (per the toggle).
  // If an actionUrl is set, jump to it in the browser on approval (action inbox).
  let approving = false;
  const approve = async () => {
    if (approving || submitted) return; // never open the URL / resolve twice
    approving = true;
    if (tdToggle.checked) {
      const ok = await invoke<boolean>("touch_id_approve");
      if (!ok) {
        approving = false;
        return; // auth cancelled/failed → keep window open
      }
    }
    // 다시 열어 "완료"를 누른 경우: 링크를 또 열지 않고 바로 resolve.
    if (resuming) {
      sendDecision("approved", undefined, true);
      return;
    }
    if (p.actionUrl) {
      if (p.configOpenUrl ?? true) {
        await invoke("open_url", { url: p.actionUrl }).catch(() => {});
      } else {
        // Toggle OFF: skip auto-jump. Copy URL so owner can open manually
        // without losing the action target. Console log as a fallback for
        // headless/clipboard-denied environments.
        console.log("knock: action URL (auto-open disabled):", p.actionUrl);
        try {
          await navigator.clipboard.writeText(p.actionUrl);
        } catch {
          // clipboard unavailable (no user gesture, denied) — ignore
        }
      }
    }
    if (checklistMode) {
      // resolve 하지 않는다. 큐에 남겨 두고 창만 닫아 브라우저를 가리지 않는다.
      sink.startAction!();
      return;
    }
    sendDecision("approved");
  };

  const submitFeedback = () => {
    const txt = feedback.value.trim();
    if (txt) sendDecision("annotated", txt);
  };

  optApprove.addEventListener("click", approve);
  optCancel.addEventListener("click", () => sendDecision("dismissed"));
  sendBtn.addEventListener("click", submitFeedback);
  feedback.addEventListener("input", () => {
    sendBtn.disabled = feedback.value.trim().length === 0;
  });

  // Clipboard image paste → save to a temp file, append its path to the feedback.
  feedback.addEventListener("paste", (e) => {
    const items = e.clipboardData?.items;
    if (!items) return;
    for (const item of items) {
      if (item.type.startsWith("image/")) {
        e.preventDefault();
        const blob = item.getAsFile();
        if (!blob) continue;
        const reader = new FileReader();
        reader.onload = async () => {
          try {
            const path = await invoke<string>("save_pasted_image", {
              dataUrl: reader.result as string,
            });
            feedback.value +=
              (feedback.value ? "\n" : "") + `[붙여넣은 이미지: ${path}]`;
            sendBtn.disabled = false;
          } catch {
            /* ignore */
          }
        };
        reader.readAsDataURL(blob);
      }
    }
  });

  // Keyboard: 1/2/3, ↑↓ move, Enter/Space run, Cmd+Enter submit, Esc cancel/close.
  const opts = [optApprove, optCancel].filter(
    (o) => !o.classList.contains("hidden"),
  );
  let focusIdx = 0;
  opts[0]?.focus();
  $("annotate-footer").addEventListener("focusin", (e) => {
    const i = opts.indexOf(e.target as HTMLElement);
    if (i >= 0) focusIdx = i;
  });

  const run = (o: HTMLElement) => {
    if (o === optApprove) approve();
    else sendDecision("dismissed");
  };

  setKey((e) => {
    const inText = document.activeElement === feedback;
    if ((e.metaKey || e.ctrlKey) && e.key === "Enter") {
      e.preventDefault();
      if (feedback.value.trim()) submitFeedback();
      else if (p.gate) approve();
      return;
    }
    if (e.key === "Escape") {
      e.preventDefault();
      if (inText) (document.activeElement as HTMLElement).blur();
      else sendDecision("dismissed");
      return;
    }
    if (inText) return;
    if (e.key === "1" && p.gate) {
      e.preventDefault();
      approve();
    } else if (e.key === "2") {
      e.preventDefault();
      feedback.focus();
    } else if (e.key === "3") {
      e.preventDefault();
      sendDecision("dismissed");
    } else if (e.key === "ArrowDown") {
      e.preventDefault();
      focusIdx = (focusIdx + 1) % opts.length;
      opts[focusIdx].focus();
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      focusIdx = (focusIdx - 1 + opts.length) % opts.length;
      opts[focusIdx].focus();
    } else if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      run(opts[focusIdx]);
    }
  });
}

// =====================================================================
// ask mode — one question at a time (wizard), arrow-key navigation
// =====================================================================
const OTHER = -1;

interface QState {
  selected: Set<number>;
  other: boolean;
  otherText: string;
}

function setupAsk(p: AskPayload) {
  const badgeEl = $("badge");
  $("title").textContent = p.title;

  const root = $("ask-root");
  root.classList.remove("hidden");
  $("ask-footer").classList.remove("hidden");
  wireTtsHeader(p.configTts);

  // Header Touch ID toggle — same config as annotate; gates the final submit.
  const tdWrap = $("td-toggle-wrap");
  const tdToggle = $<HTMLInputElement>("td-toggle");
  tdWrap.classList.remove("hidden");
  tdWrap.title = "이 요청과 이후 요청에 Touch ID 요구";
  tdToggle.disabled = false;
  tdToggle.checked = p.configTouchId ?? false;
  tdToggle.addEventListener("change", () => {
    invoke("save_touch_id", { enabled: tdToggle.checked });
  });

  // Optional background/context markdown, rendered at the top of the same
  // scroll area as the questions (so long context + options scroll together).
  if (p.contextHtml) {
    const ctx = el("div", "ask-context markdown");
    ctx.innerHTML = p.contextHtml;
    root.appendChild(ctx);
    makeLinksExternal(ctx);
    decorateEmbeds(ctx);
  }

  const prevBtn = $<HTMLButtonElement>("ask-prev");
  const nextBtn = $<HTMLButtonElement>("ask-next");
  const submitBtn = $<HTMLButtonElement>("ask-submit");

  const qs = p.questions?.questions ?? [];
  const N = qs.length;
  const fastSubmit = N === 1;
  const contextLength = p.contextHtml?.length ?? 0;
  void applyWindowLayout(
    fastSubmit && contextLength < 3000
      ? "ask-single"
      : N > 2 || contextLength > 5000
        ? "large"
        : "compact",
  );
  const qstate: QState[] = qs.map(() => ({
    selected: new Set<number>(),
    other: false,
    otherText: "",
  }));

  const keyFor = (q: AskQuestion, qi: number) =>
    q.header || q.question || `q${qi}`;

  const answeredFor = (qi: number) => {
    const st = qstate[qi];
    if (st.other && st.otherText.trim()) return true;
    return st.selected.size > 0;
  };
  const allAnswered = () => qs.every((_, qi) => answeredFor(qi));

  const labelsFor = (qi: number): string[] => {
    const q = qs[qi];
    const st = qstate[qi];
    const out = [...st.selected].map((i) => q.options![i].label);
    if (st.other && st.otherText.trim()) out.push(st.otherText.trim());
    return out;
  };

  // --- build one section per question ---
  const sections: HTMLElement[] = [];
  qs.forEach((q, qi) => {
    const multi = q.multiSelect === true;
    const sec = el("section", "ask-q hidden");
    if (q.question) sec.appendChild(el("h2", "ask-q-title", q.question));
    sec.appendChild(
      el(
        "p",
        "ask-hint",
        multi
          ? `복수 선택 가능 — 숫자/Space 로 토글, ${fastSubmit ? "Enter 로 제출" : "→ 또는 Enter 로 다음"}`
          : `하나 선택 — 숫자/Space 로 선택, ${fastSubmit ? "Enter 로 제출" : "→ 또는 Enter 로 다음"}`,
      ),
    );

    const optsWrap = el("div", "ask-options");
    const allOpts = [
      ...(q.options ?? []).map((o, i) => ({
        i,
        label: o.label,
        desc: o.description,
      })),
      { i: OTHER, label: "기타", desc: "직접 입력" },
    ];

    let otherInput: HTMLInputElement | null = null;

    allOpts.forEach((o, di) => {
      const optEl = el("label", "ask-opt") as HTMLLabelElement;
      optEl.tabIndex = 0;
      optEl.dataset.qi = String(qi);
      optEl.appendChild(el("span", "ask-opt-num", String(di + 1)));
      const input = document.createElement("input");
      input.type = multi ? "checkbox" : "radio";
      input.name = `q${qi}`;
      input.tabIndex = -1;
      optEl.appendChild(input);
      const body = el("div", "ask-opt-body");
      body.appendChild(el("div", "ask-opt-label", o.label));
      if (o.desc) body.appendChild(el("div", "ask-opt-desc", o.desc));
      optEl.appendChild(body);
      optsWrap.appendChild(optEl);

      if (o.i === OTHER) {
        otherInput = document.createElement("input");
        otherInput.type = "text";
        otherInput.className = "ask-other hidden";
        otherInput.placeholder = "직접 입력...";
        optsWrap.appendChild(otherInput);
        otherInput.addEventListener("input", () => {
          qstate[qi].otherText = otherInput!.value;
          refreshNav();
        });
      }

      input.addEventListener("change", () => {
        const st = qstate[qi];
        if (!multi) {
          st.selected.clear();
          st.other = false;
          otherInput?.classList.add("hidden");
        }
        if (o.i === OTHER) {
          st.other = input.checked;
          otherInput?.classList.toggle("hidden", !input.checked);
          if (input.checked) otherInput?.focus();
          else st.otherText = "";
        } else if (multi) {
          if (input.checked) st.selected.add(o.i);
          else st.selected.delete(o.i);
        } else {
          st.selected = new Set([o.i]);
        }
        refreshNav();
      });
    });

    sec.appendChild(optsWrap);
    sections.push(sec);
    root.appendChild(sec);
  });

  // --- summary section ---
  const summary = el("section", "ask-q ask-summary hidden");
  summary.appendChild(el("h2", "ask-q-title", "선택 내용 확인"));
  const summaryList = el("div", "summary-list");
  summary.appendChild(summaryList);

  // Opt-in: also emit a single-use execution approval so the next knock
  // permission gate auto-approves (owner pre-authorization). Default off.
  const grantWrap = el("label", "ask-grant") as HTMLLabelElement;
  const grantCb = document.createElement("input");
  grantCb.type = "checkbox";
  grantWrap.appendChild(grantCb);
  grantWrap.appendChild(
    el(
      "span",
      "ask-grant-label",
      "이 선택을 실행 승인으로 함께 전송 — 다음 knock 게이트 1회 자동 승인 (5분)",
    ),
  );
  if (fastSubmit) {
    sections[0]?.appendChild(grantWrap);
  } else {
    summary.appendChild(grantWrap);
    sections.push(summary);
    root.appendChild(summary);
  }

  const renderSummary = () => {
    summaryList.innerHTML = "";
    qs.forEach((q, qi) => {
      const row = el("div", "summary-row");
      row.appendChild(el("div", "summary-q", keyFor(q, qi)));
      const vals = labelsFor(qi);
      row.appendChild(
        el("div", "summary-a", vals.length ? vals.join(", ") : "(미선택)"),
      );
      summaryList.appendChild(row);
    });
  };

  // --- wizard navigation ---
  let step = 0; // 0..N-1 = questions, N = summary
  const sectionOpts = (i: number) =>
    [...sections[i].querySelectorAll<HTMLElement>(".ask-opt")];
  let focusIdx = 0;

  const refreshNav = () => {
    const onSummary = !fastSubmit && step === N;
    if (!onSummary) {
      nextBtn.disabled = !answeredFor(step);
      if (fastSubmit) submitBtn.disabled = !allAnswered();
    }
  };

  const showStep = (i: number) => {
    const maxStep = fastSubmit ? 0 : N;
    step = Math.max(0, Math.min(maxStep, i));
    sections.forEach((s, si) => s.classList.toggle("hidden", si !== step));
    const onSummary = !fastSubmit && step === N;

    badgeEl.textContent = onSummary ? "확인" : N === 1 ? "질문" : `질문 ${step + 1} / ${N}`;
    prevBtn.classList.toggle("hidden", step === 0);
    nextBtn.classList.toggle("hidden", onSummary || fastSubmit);
    submitBtn.classList.toggle("hidden", !(onSummary || fastSubmit));

    if (onSummary) {
      renderSummary();
      submitBtn.disabled = !allAnswered();
      submitBtn.focus();
    } else {
      nextBtn.disabled = !answeredFor(step);
      if (fastSubmit) submitBtn.disabled = !allAnswered();
      // focus the selected option, or the first one
      const opts = sectionOpts(step);
      const sel = opts.findIndex((o) =>
        o.querySelector<HTMLInputElement>("input")?.checked,
      );
      focusIdx = sel >= 0 ? sel : 0;
      opts[focusIdx]?.focus();
    }
  };

  const goNext = () => {
    if (step < N && !answeredFor(step)) return;
    if (fastSubmit) {
      void doSubmit();
      return;
    }
    showStep(step + 1);
  };
  const goPrev = () => showStep(step - 1);

  const doSubmit = async () => {
    if (!allAnswered()) {
      if (!fastSubmit) showStep(N);
      return;
    }
    if (tdToggle.checked) {
      const ok = await invoke<boolean>("touch_id_approve");
      if (!ok) return; // auth cancelled/failed → keep window open
    }
    // The stdout contract remains arrays for both single and multi-select.
    const answers: Record<string, string[]> = {};
    qs.forEach((q, qi) => {
      answers[keyFor(q, qi)] = labelsFor(qi);
    });
    once(() => sink.ask(answers, grantCb.checked));
  };

  prevBtn.addEventListener("click", goPrev);
  nextBtn.addEventListener("click", goNext);
  submitBtn.addEventListener("click", doSubmit);
  $("ask-dismiss").addEventListener("click", () =>
    once(() => sink.dismiss()),
  );

  root.addEventListener("focusin", (e) => {
    const opt = (e.target as HTMLElement).closest(".ask-opt");
    if (opt && step < N) focusIdx = sectionOpts(step).indexOf(opt as HTMLElement);
  });

  // --- keyboard ---
  setKey((e) => {
    const tgt = e.target as HTMLElement | null;
    const inText =
      !!tgt &&
      tgt.tagName === "INPUT" &&
      (tgt as HTMLInputElement).type === "text";

    if ((e.metaKey || e.ctrlKey) && e.key === "Enter") {
      e.preventDefault();
      doSubmit();
      return;
    }
    if (e.key === "Escape") {
      e.preventDefault();
      once(() => sink.dismiss());
      return;
    }

    // Summary step
    if (!fastSubmit && step === N) {
      if (e.key === "ArrowLeft") {
        e.preventDefault();
        goPrev();
      } else if (e.key === "Enter") {
        e.preventDefault();
        doSubmit();
      }
      return;
    }

    if (inText) {
      // While typing in 기타, only Enter advances.
      if (e.key === "Enter") {
        e.preventDefault();
        goNext();
      }
      return;
    }

    const opts = sectionOpts(step);
    if (e.key === "ArrowDown") {
      e.preventDefault();
      focusIdx = (focusIdx + 1) % opts.length;
      opts[focusIdx]?.focus();
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      focusIdx = (focusIdx - 1 + opts.length) % opts.length;
      opts[focusIdx]?.focus();
    } else if (e.key === "ArrowRight") {
      e.preventDefault();
      goNext();
    } else if (e.key === "ArrowLeft") {
      e.preventDefault();
      goPrev();
    } else if (e.key === "Enter") {
      // progress with Enter or → ; Space only selects/toggles
      e.preventDefault();
      goNext();
    } else if (e.key === " ") {
      // Space selects (single) or toggles (multi); advance with → / Enter
      e.preventDefault();
      opts[focusIdx]?.querySelector("input")?.click();
    } else if (/^[1-9]$/.test(e.key)) {
      e.preventDefault();
      const n = parseInt(e.key, 10);
      if (opts[n - 1]) {
        focusIdx = n - 1;
        opts[n - 1].querySelector("input")?.click();
        opts[n - 1].focus();
        // select only — advance with Enter / → / second Space
      }
    }
  });

  showStep(0);
}

// =====================================================================
function setupSettings(p: SettingsPayload) {
  void applyWindowLayout("settings");
  $("badge").textContent = "설정";
  $("title").textContent = "Knock 설정";
  $("settings-root").classList.remove("hidden");
  $("settings-footer").classList.remove("hidden");

  const toggle = $<HTMLInputElement>("touch-id-toggle");
  toggle.checked = p.touchId;
  toggle.addEventListener("change", () => {
    invoke("save_touch_id", { enabled: toggle.checked });
  });

  const ttsToggle = $<HTMLInputElement>("tts-toggle");
  const ttsOptions = $("tts-options");
  const ttsScope = $<HTMLSelectElement>("tts-scope");
  const ttsStyle = $<HTMLSelectElement>("tts-style");
  const ttsVoice = $<HTMLSelectElement>("tts-voice");
  const ttsRepeat = $<HTMLInputElement>("tts-repeat");
  const ttsPhrase = $<HTMLInputElement>("tts-phrase");
  const phraseRow = $("tts-phrase-row");
  const repeatRow = $("tts-repeat-row");

  // Reflect saved values.
  ttsToggle.checked = p.tts ?? false;
  ttsScope.value = p.ttsScope ?? "title";
  ttsStyle.value = p.ttsStyle ?? "plain";
  ttsVoice.value = p.ttsVoice ?? "";
  ttsRepeat.value = String(p.ttsRepeat ?? 3);
  ttsPhrase.value = p.ttsPhrase ?? "";

  // Show options only when TTS is on; delivery-only rows only in delivery style.
  const syncVisibility = () => {
    ttsOptions.classList.toggle("hidden", !ttsToggle.checked);
    const delivery = ttsStyle.value === "delivery";
    phraseRow.classList.toggle("hidden", !delivery);
    repeatRow.classList.toggle("hidden", !delivery);
  };
  syncVisibility();

  ttsToggle.addEventListener("change", () => {
    invoke("save_tts", { enabled: ttsToggle.checked });
    syncVisibility();
  });
  ttsScope.addEventListener("change", () =>
    invoke("save_tts_opt", { key: "tts_scope", value: ttsScope.value }),
  );
  ttsStyle.addEventListener("change", () => {
    invoke("save_tts_opt", { key: "tts_style", value: ttsStyle.value });
    syncVisibility();
  });
  ttsVoice.addEventListener("change", () =>
    invoke("save_tts_opt", { key: "tts_voice", value: ttsVoice.value }),
  );
  ttsRepeat.addEventListener("change", () =>
    invoke("save_tts_opt", {
      key: "tts_repeat",
      value: Math.min(10, Math.max(1, parseInt(ttsRepeat.value) || 3)),
    }),
  );
  ttsPhrase.addEventListener("change", () =>
    invoke("save_tts_opt", { key: "tts_phrase", value: ttsPhrase.value }),
  );

  if (p.version) $("version-tag").textContent = `v${p.version}`;
  $("report-bug").addEventListener("click", (e) => {
    e.preventDefault();
    invoke("open_url", {
      url: "https://github.com/hihenen/knock/issues/new/choose",
    }).catch(() => {});
  });
  $("release-notes").addEventListener("click", (e) => {
    e.preventDefault();
    invoke("open_url", {
      url: "https://github.com/hihenen/knock/releases",
    }).catch(() => {});
  });

  const close = () => once(() => invoke("dismiss"));
  $("settings-close").addEventListener("click", close);
  setKey((e) => {
    if (e.key === "Escape" || ((e.metaKey || e.ctrlKey) && e.key === "Enter")) {
      e.preventDefault();
      close();
    }
  });
}

// =====================================================================
// daemon mode — one window, a queue of requests from many sessions
// =====================================================================
// True while a detail (annotate/ask) is open, so re-renders don't wipe input.
let daemonBusy = false;

function daemonSink(id: string): Sink {
  const resolve = (
    decision: string,
    feedback: string | null,
    answers: Record<string, string | string[]> | null,
    grant: boolean = false,
    completed: boolean | null = null,
  ) => {
    invoke("daemon_resolve", { id, decision, feedback, answers, grant, completed });
    daemonBusy = false;
    submitted = false;
    // Re-render in place (no page reload) to show the next item / the list.
    setTimeout(() => void renderDaemon(), 80);
  };
  return {
    annotate: (d, f, completed) =>
      resolve(d, f ?? null, null, false, completed ?? null),
    ask: (a, grant) => resolve("answered", null, a, !!grant),
    dismiss: () => resolve("dismissed", null, null),
    // resolve 하지 않는다 — 큐에 "진행 중"으로 남기고 창만 닫는다.
    startAction: () => {
      invoke("daemon_start_action", { id });
      daemonBusy = false;
      submitted = false;
      invoke("hide_window");
      setTimeout(() => void renderDaemon(), 80);
    },
  };
}

function openDetail(item: QueueItem) {
  daemonBusy = true;
  resetView();
  sink = daemonSink(item.id);
  if (item.kind === "ask") setupAsk(item.payload as AskPayload);
  else
    setupAnnotate({
      ...(item.payload as AnnotatePayload),
      inProgress: item.inProgress ?? false,
    });
}

function renderList(items: QueueItem[]) {
  resetView();
  void applyWindowLayout("queue", items.length);
  $("badge").textContent =
    items.length === 0 ? "대기 없음" : `대기 ${items.length}건`;
  $("title").textContent = "승인 대기 목록";
  const content = $("content");
  content.classList.remove("hidden");
  content.innerHTML = "";

  if (items.length === 0) {
    content.appendChild(el("p", "queue-empty", "대기 중인 요청이 없습니다."));
    return;
  }
  const list = el("div", "queue-list");
  items.forEach((item, index) => {
    const card = el("div", "queue-card");
    card.tabIndex = 0;
    card.appendChild(el("span", "queue-num", String(index + 1)));
    card.appendChild(
      el(
        "span",
        item.inProgress ? "queue-kind queue-kind-progress" : "queue-kind",
        item.inProgress ? "진행 중" : item.kind === "ask" ? "질문" : "승인",
      ),
    );
    const copy = el("span", "queue-copy");
    copy.appendChild(el("span", "queue-title", item.title));
    const source = [item.source?.project, item.source?.caller]
      .filter((v, i, a): v is string => !!v && a.indexOf(v) === i)
      .join(" · ");
    const ageSeconds = item.createdAt
      ? Math.max(0, Math.floor(Date.now() / 1000) - item.createdAt)
      : 0;
    const age = !item.createdAt
      ? ""
      : ageSeconds < 60
        ? "방금"
        : ageSeconds < 3600
          ? `${Math.floor(ageSeconds / 60)}분 전`
          : `${Math.floor(ageSeconds / 3600)}시간 전`;
    const meta = [source, age].filter(Boolean).join(" · ");
    if (meta) copy.appendChild(el("span", "queue-meta", meta));
    card.appendChild(copy);
    const open = () => openDetail(item);
    card.addEventListener("click", open);
    card.addEventListener("keydown", (e) => {
      if (e.key === "Enter" || e.key === " ") {
        e.preventDefault();
        open();
      }
    });
    list.appendChild(card);
  });
  content.appendChild(list);

  // Match the detail views' number-key navigation: 1-9 opens that queued
  // request immediately. Items after 9 remain available by click/Tab+Enter.
  setKey((e) => {
    if (!/^[1-9]$/.test(e.key)) return;
    const index = parseInt(e.key, 10) - 1;
    const item = items[index];
    if (!item) return;
    e.preventDefault();
    openDetail(item);
  });
}

// Pull the current queue and render in place. Skipped while a detail is open
// so it doesn't blow away what the user is typing.
async function renderDaemon() {
  if (daemonBusy) return;
  let q: QueuePayload;
  try {
    q = await invoke<QueuePayload>("daemon_queue");
  } catch {
    return;
  }
  if (!q || q.mode !== "queue") return;
  if (q.items.length === 1) {
    try {
      openDetail(q.items[0]);
    } catch (e) {
      // Safety net: never leave a blank window — surface the error.
      console.error(e);
      const c = $("content");
      c.classList.remove("hidden");
      c.textContent = "표시 중 오류가 발생했습니다. knock 을 다시 실행해 주세요.";
    }
    return;
  }
  renderList(q.items);
}

async function init() {
  // Non-blocking, fail-silent update-available check (shows a dismissible banner).
  void checkUpdateBanner();
  // Remember whatever size the owner drags the window to, per layout.
  watchManualResize();
  // Daemon first: if a queue command answers, we're the single-window daemon.
  try {
    const q = await invoke<QueuePayload>("daemon_queue");
    if (q && q.mode === "queue") {
      await listen("native-close-requested", () => {
        // In detail view the native close button means dismiss this request;
        // in list/empty view it only hides the resident daemon window.
        if (daemonBusy) sink.dismiss();
        invoke("hide_window").catch(() => {});
      });
      await renderDaemon();
      // 외부 컨트롤러(Stream Deck 등) — 소켓으로 들어온 focus/approve 를 처리한다.
      //
      // approve 는 여기서 승인 버튼을 "누르는" 것으로 끝난다. 별도 승인 경로를
      // 만들지 않는 게 핵심이다 — 기존 클로저를 그대로 타야 Touch ID 정책도
      // 같이 따라온다. 물리 키를 잘못 눌러도 화면 승인과 똑같은 게이트를 지난다.
      await listen<{ target: string; approve: boolean; skipTouchId?: boolean }>(
        "external-control",
        async (evt) => {
          const { target, approve, skipTouchId } = evt.payload ?? {
            target: "",
            approve: false,
          };
          let cur: QueuePayload;
          try {
            cur = await invoke<QueuePayload>("daemon_queue");
          } catch {
            return;
          }
          if (!cur || cur.mode !== "queue") return;
          const item = target
            ? cur.items.find((i) => i.id === target)
            : cur.items[0];
          if (!item) return;
          daemonBusy = false; // 다른 항목이 열려 있어도 물리 키 의도를 우선한다
          openDetail(item);
          if (approve) {
            // 렌더가 끝난 뒤에 눌러야 리스너가 붙어 있다.
            setTimeout(() => {
              // config 에서 켠 경우에만 Touch ID 를 건너뛴다. 토글을 끄는 방식이라
              // 승인 경로 자체는 그대로다 — 별도 우회로를 만들지 않는 게 중요하다.
              //
              // 단 토글이 disabled 면 손대지 않는다. disabled == 명시적 --touch-id
              // 요청(prd 변경, destructive 등)이고, 그건 사람도 화면에서 끌 수 없다.
              // `disabled` 는 사람의 클릭만 막을 뿐 스크립트의 .checked 는 막지
              // 못하므로, 여기서 걸러내지 않으면 물리 키로 critical 게이트의 생체
              // 인증이 통째로 면제된다.
              if (skipTouchId) {
                const td = $<HTMLInputElement>("td-toggle");
                if (td && !td.disabled) td.checked = false;
              }
              $("opt-approve").click();
            }, 80);
          }
        },
      );
      // Physical scroll keys (Stream Deck) drive the same body the trackpad does.
      listen<{ dir: "up" | "down" }>("scroll-request", (e) =>
        scrollBody(e.payload?.dir === "up" ? "up" : "down"),
      );
      // Event-driven refresh + a slow poll as a backstop for missed events.
      listen("queue-changed", () => void renderDaemon());
      setInterval(() => void renderDaemon(), 2000);
      return;
    }
  } catch {
    /* not the daemon → legacy single-shot window */
  }

  let payload: Payload;
  try {
    payload = await invoke<Payload>("get_payload");
  } catch {
    return;
  }
  if (payload.mode === "ask") setupAsk(payload);
  else if (payload.mode === "settings") setupSettings(payload);
  else setupAnnotate(payload);
}

window.addEventListener("DOMContentLoaded", init);
