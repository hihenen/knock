import { invoke } from "@tauri-apps/api/core";

interface AnnotatePayload {
  mode: "annotate";
  html: string;
  title: string;
  gate: boolean;
  touchId?: boolean;
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
}
interface SettingsPayload {
  mode: "settings";
  touchId: boolean;
}
type Payload = AnnotatePayload | AskPayload | SettingsPayload;

const $ = <T extends HTMLElement>(id: string) =>
  document.getElementById(id) as T;
const el = (tag: string, cls?: string, text?: string) => {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text) e.textContent = text;
  return e;
};

let submitted = false;
function once(fn: () => void) {
  if (submitted) return;
  submitted = true;
  fn();
}

// =====================================================================
// annotate mode
// =====================================================================
function sendDecision(
  decision: "approved" | "annotated" | "dismissed",
  feedback?: string,
) {
  once(() => invoke("submit", { decision, feedback: feedback ?? null }));
}

function setupAnnotate(p: AnnotatePayload) {
  $("badge").textContent = "승인 요청";
  $("title").textContent = p.title;
  $("content").innerHTML = p.html;
  $("content").classList.remove("hidden");
  $("annotate-footer").classList.remove("hidden");

  const optApprove = $("opt-approve");
  const optCancel = $("opt-cancel");
  const feedback = $<HTMLTextAreaElement>("feedback");
  const sendBtn = $<HTMLButtonElement>("send");

  if (!p.gate) optApprove.classList.add("hidden");

  if (p.touchId) {
    const label = optApprove.querySelector(".ask-opt-label");
    if (label) label.textContent = "🔒 Touch ID 승인";
  }
  // Approve, optionally gated behind Touch ID / Windows Hello.
  const approve = async () => {
    if (p.touchId) {
      const ok = await invoke<boolean>("touch_id_approve");
      if (!ok) return; // auth cancelled/failed → keep window open
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

  window.addEventListener("keydown", (e) => {
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
    const multi = !!q.multiSelect;
    const sec = el("section", "ask-q hidden");
    if (q.question) sec.appendChild(el("h2", "ask-q-title", q.question));
    if (multi)
      sec.appendChild(
        el("p", "ask-hint", "복수 선택 — 숫자/Space 로 토글, → 또는 Enter 로 다음"),
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

  const doSubmit = () => {
    if (!allAnswered()) {
      showStep(N);
      return;
    }
    const answers: Record<string, string | string[]> = {};
    qs.forEach((q, qi) => {
      const vals = labelsFor(qi);
      answers[keyFor(q, qi)] = q.multiSelect ? vals : vals[0] ?? "";
    });
    once(() => invoke("submit_answers", { answers }));
  };

  prevBtn.addEventListener("click", goPrev);
  nextBtn.addEventListener("click", goNext);
  submitBtn.addEventListener("click", doSubmit);
  $("ask-dismiss").addEventListener("click", () =>
    once(() => invoke("dismiss")),
  );

  root.addEventListener("focusin", (e) => {
    const opt = (e.target as HTMLElement).closest(".ask-opt");
    if (opt && step < N) focusIdx = sectionOpts(step).indexOf(opt as HTMLElement);
  });

  // --- keyboard ---
  window.addEventListener("keydown", (e) => {
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
      once(() => invoke("dismiss"));
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

  const close = () => once(() => invoke("dismiss"));
  $("settings-close").addEventListener("click", close);
  window.addEventListener("keydown", (e) => {
    if (e.key === "Escape" || ((e.metaKey || e.ctrlKey) && e.key === "Enter")) {
      e.preventDefault();
      close();
    }
  });
}

async function init() {
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
