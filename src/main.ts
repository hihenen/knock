import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

interface AnnotatePayload {
  mode: "annotate";
  html: string;
  title: string;
  gate: boolean;
  touchId?: boolean;
  configTouchId?: boolean;
  actionUrl?: string | null;
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
}
interface SettingsPayload {
  mode: "settings";
  touchId: boolean;
  version?: string;
}
type Payload = AnnotatePayload | AskPayload | SettingsPayload;

interface QueueItem {
  id: string;
  kind: "annotate" | "ask";
  title: string;
  payload: AnnotatePayload | AskPayload;
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
  annotate: (decision: string, feedback?: string) => void;
  ask: (answers: Record<string, string | string[]>) => void;
  dismiss: () => void;
};
let sink: Sink = {
  annotate: (decision, feedback) =>
    invoke("submit", { decision, feedback: feedback ?? null }),
  ask: (answers) => invoke("submit_answers", { answers }),
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

// Open http(s) links from rendered markdown in the real browser, not the webview.
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

// Hide every mode section + footer and reset per-view state before rendering
// the next one (daemon window is reused across requests).
function resetView() {
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
) {
  once(() => sink.annotate(decision, feedback));
}

function setupAnnotate(p: AnnotatePayload) {
  $("badge").textContent = "승인 요청";
  $("title").textContent = p.title;
  $("content").innerHTML = p.html;
  $("content").classList.remove("hidden");
  makeLinksExternal($("content"));
  $("annotate-footer").classList.remove("hidden");

  const optApprove = $("opt-approve");
  const optCancel = $("opt-cancel");
  const feedback = $<HTMLTextAreaElement>("feedback");
  const sendBtn = $<HTMLButtonElement>("send");

  if (!p.gate) optApprove.classList.add("hidden");

  // Header Touch ID toggle — reflects the saved config and also applies to this
  // approval. Flipping it persists to config (next critical gates) immediately.
  const tdWrap = $("td-toggle-wrap");
  const tdToggle = $<HTMLInputElement>("td-toggle");
  const approveLabel = optApprove.querySelector(".ask-opt-label");
  const hasAction = !!p.actionUrl;
  const reflectLabel = () => {
    if (!approveLabel) return;
    const base = tdToggle.checked ? "🔒 Touch ID 승인" : "✓ 승인";
    approveLabel.textContent = hasAction ? `${base} → 링크 열기` : base;
  };
  if (p.gate) {
    tdWrap.classList.remove("hidden");
    tdToggle.checked = p.configTouchId ?? p.touchId ?? false;
    reflectLabel();
    tdToggle.addEventListener("change", () => {
      invoke("save_touch_id", { enabled: tdToggle.checked });
      reflectLabel();
    });
  }

  // Approve, optionally gated behind Touch ID / Windows Hello (per the toggle).
  // If an actionUrl is set, jump to it in the browser on approval (action inbox).
  const approve = async () => {
    if (tdToggle.checked) {
      const ok = await invoke<boolean>("touch_id_approve");
      if (!ok) return; // auth cancelled/failed → keep window open
    }
    if (p.actionUrl) {
      await invoke("open_url", { url: p.actionUrl }).catch(() => {});
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

  // Header Touch ID toggle — same config as annotate; gates the final submit.
  const tdWrap = $("td-toggle-wrap");
  const tdToggle = $<HTMLInputElement>("td-toggle");
  tdWrap.classList.remove("hidden");
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
  }

  const prevBtn = $<HTMLButtonElement>("ask-prev");
  const nextBtn = $<HTMLButtonElement>("ask-next");
  const submitBtn = $<HTMLButtonElement>("ask-submit");

  const qs = p.questions?.questions ?? [];
  const N = qs.length;
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
    // Always multi-select (checkboxes): real use is "pick 1-2 options and/or
    // add a note in 기타", which a single radio can't express. More flexible.
    const multi = true;
    const sec = el("section", "ask-q hidden");
    if (q.question) sec.appendChild(el("h2", "ask-q-title", q.question));
    sec.appendChild(
      el("p", "ask-hint", "복수 선택 가능 — 숫자/Space 로 토글, → 또는 Enter 로 다음"),
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
  sections.push(summary);
  root.appendChild(summary);

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
    const onSummary = step === N;
    if (!onSummary) nextBtn.disabled = !answeredFor(step);
  };

  const showStep = (i: number) => {
    step = Math.max(0, Math.min(N, i));
    sections.forEach((s, si) => s.classList.toggle("hidden", si !== step));
    const onSummary = step === N;

    badgeEl.textContent = onSummary ? "확인" : `질문 ${step + 1} / ${N}`;
    prevBtn.classList.toggle("hidden", step === 0);
    nextBtn.classList.toggle("hidden", onSummary);
    submitBtn.classList.toggle("hidden", !onSummary);

    if (onSummary) {
      renderSummary();
      submitBtn.disabled = !allAnswered();
      submitBtn.focus();
    } else {
      nextBtn.disabled = !answeredFor(step);
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
    showStep(step + 1);
  };
  const goPrev = () => showStep(step - 1);

  const doSubmit = async () => {
    if (!allAnswered()) {
      showStep(N);
      return;
    }
    if (tdToggle.checked) {
      const ok = await invoke<boolean>("touch_id_approve");
      if (!ok) return; // auth cancelled/failed → keep window open
    }
    // Always multi-select → answers are always string arrays (selected labels
    // plus any 기타 free text).
    const answers: Record<string, string[]> = {};
    qs.forEach((q, qi) => {
      answers[keyFor(q, qi)] = labelsFor(qi);
    });
    once(() => sink.ask(answers));
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
    if (step === N) {
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
  $("badge").textContent = "설정";
  $("title").textContent = "Knock 설정";
  $("settings-root").classList.remove("hidden");
  $("settings-footer").classList.remove("hidden");

  const toggle = $<HTMLInputElement>("touch-id-toggle");
  toggle.checked = p.touchId;
  toggle.addEventListener("change", () => {
    invoke("save_touch_id", { enabled: toggle.checked });
  });

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
  ) => {
    invoke("daemon_resolve", { id, decision, feedback, answers });
    daemonBusy = false;
    submitted = false;
    // Re-render in place (no page reload) to show the next item / the list.
    setTimeout(() => void renderDaemon(), 80);
  };
  return {
    annotate: (d, f) => resolve(d, f ?? null, null),
    ask: (a) => resolve("answered", null, a),
    dismiss: () => resolve("dismissed", null, null),
  };
}

function openDetail(item: QueueItem) {
  daemonBusy = true;
  resetView();
  sink = daemonSink(item.id);
  if (item.kind === "ask") setupAsk(item.payload as AskPayload);
  else setupAnnotate(item.payload as AnnotatePayload);
}

function renderList(items: QueueItem[]) {
  resetView();
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
  items.forEach((item) => {
    const card = el("div", "queue-card");
    card.tabIndex = 0;
    card.appendChild(el("span", "queue-kind", item.kind === "ask" ? "질문" : "승인"));
    card.appendChild(el("span", "queue-title", item.title));
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
  // Daemon first: if a queue command answers, we're the single-window daemon.
  try {
    const q = await invoke<QueuePayload>("daemon_queue");
    if (q && q.mode === "queue") {
      await renderDaemon();
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
