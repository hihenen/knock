// knock Stream Deck 플러그인
//
// Stream Deck 이 이 프로세스를 띄우면서 -port/-pluginUUID/-registerEvent/-info 를
// 인자로 준다. 그 포트의 WebSocket 에 붙어 registerEvent 로 등록하면, 이후 키
// 이벤트가 오고 우리는 setTitle/setState 로 키를 그린다.
//
// 세 가지 액션을 제공한다.
//   slot     큐의 N번째를 맡는 키. 제목에 대기 내용이 뜨고, 눌러 열거나 처리한다
//   approve  맨 앞 요청 승인
//   dismiss  맨 앞 요청 취소
//
// 승인이 이 플러그인에서 완결되지 않는다는 점이 중요하다. knock 이 창을 띄우고
// 기존 승인 경로를 타므로 Touch ID 정책이 그대로 걸린다. 키를 잘못 눌러도
// 화면 승인과 똑같은 게이트를 지난다.

import WebSocket from "ws";
import * as knock from "./knock.js";
import * as orca from "./orca.js";

const argv = process.argv;
const arg = (name) => {
  const i = argv.indexOf(name);
  return i === -1 ? null : argv[i + 1];
};

const port = arg("-port");
const uuid = arg("-pluginUUID");
const registerEvent = arg("-registerEvent");
if (!port || !uuid || !registerEvent) {
  // Stream Deck 이 띄운 게 아니다. 손으로 실행하면 여기로 온다.
  console.error("Stream Deck 이 전달하는 -port/-pluginUUID/-registerEvent 가 없습니다.");
  process.exit(1);
}

const ws = new WebSocket(`ws://127.0.0.1:${port}`);
const send = (o) => {
  if (ws.readyState === WebSocket.OPEN) ws.send(JSON.stringify(o));
};

// context(키 하나의 인스턴스 id) -> { action, slot }
const keys = new Map();

// orca 조회는 knock 소켓보다 무겁다(190KB / 0.3s 실측). 키 렌더는 1초마다 돌지만
// 세션 목록은 그보다 드물게 갱신하고 그 사이에는 캐시를 쓴다.
let sessionCache = { items: [], alive: false, at: 0 };
const SESSION_TTL_MS = 6000;
async function getSessions() {
  if (Date.now() - sessionCache.at < SESSION_TTL_MS) return sessionCache;
  const r = await orca.sessions();
  sessionCache = { ...r, at: Date.now() };
  return sessionCache;
}

const setTitle = (context, title) =>
  send({ event: "setTitle", context, payload: { title, target: 0 } });
const setState = (context, state) =>
  send({ event: "setState", context, payload: { state } });
const showAlert = (context) => send({ event: "showAlert", context });

/**
 * 키에 무엇을 쓸지.
 *
 * 제목(파일명)보다 **어느 세션이 요청했는지**가 먼저다. 여러 Claude Code / Codex
 * 세션이 동시에 승인을 기다릴 때 구분해야 할 것은 그쪽이기 때문이다. 출처를 모르면
 * 그때 제목으로 떨어진다.
 */
function slotLabel(it) {
  const project = it.source?.project;
  const caller = it.source?.caller;
  const head = project || caller || it.title || "";
  const tail = project && caller ? caller : "";
  const age = ageLabel(it.createdAt);
  return [fitTitle(head, 9, 2), tail ? fitTitle(tail, 9, 1) : "", age]
    .filter(Boolean)
    .join("\n");
}

/**
 * 세션 키에 쓸 내용 — 이름 / 상태 / 마지막 활동.
 *
 * "얼마나 돌고 있는지" 는 워크트리 생성 시각이 아니라 **마지막 출력 이후 경과**로
 * 잰다. working 인데 출력이 30분째 없으면 멈춘 것이고, 그게 봐야 할 신호다.
 */
function sessionLabel(s) {
  const mark = s.unread ? "●" : s.status === "working" ? "▶" : "·";
  const idle = s.lastOutputAt ? sinceLabel(s.lastOutputAt) : "-";
  return [fitTitle(s.name, 10, 2), `${mark} ${idle}`, `live ${s.live}`].join("\n");
}

function sinceLabel(ts) {
  const sec = Math.max(0, Math.floor((Date.now() - ts) / 1000));
  if (sec < 60) return "방금";
  if (sec < 3600) return `${Math.floor(sec / 60)}분`;
  if (sec < 86400) return `${Math.floor(sec / 3600)}시간`;
  return `${Math.floor(sec / 86400)}일`;
}

/** 얼마나 기다렸는지. 오래 묵은 요청이 눈에 띄어야 한다. */
function ageLabel(createdAt) {
  if (!createdAt) return "";
  const s = Math.max(0, Math.floor(Date.now() / 1000) - createdAt);
  if (s < 60) return "방금";
  if (s < 3600) return `${Math.floor(s / 60)}분`;
  return `${Math.floor(s / 3600)}시간`;
}

/** 제목은 키 한 칸에 들어가야 한다. 길면 자르고, 단어가 아니라 글자 기준으로 접는다. */
function fitTitle(text, perLine = 8, lines = 3) {
  const t = String(text ?? "").replace(/\s+/g, " ").trim();
  const out = [];
  for (let i = 0; i < t.length && out.length < lines; i += perLine) {
    out.push(t.slice(i, i + perLine));
  }
  if (out.length === lines && t.length > perLine * lines) {
    out[lines - 1] = out[lines - 1].slice(0, perLine - 1) + "…";
  }
  return out.join("\n");
}

/**
 * 큐를 읽어 모든 키를 다시 그린다.
 *
 * 폴링인 이유: knock 데몬이 이벤트를 밖으로 밀어주지 않는다. 소켓은 요청-응답
 * 뿐이라 우리가 주기적으로 물어야 한다. 1초면 사람이 느끼기에 즉시고, 로컬
 * 소켓이라 비용도 사실상 없다.
 */
async function refresh() {
  const { items, alive } = await knock.list();
  const needSessions = [...keys.values()].some((m) => m.action === "session");
  const sess = needSessions ? await getSessions() : { items: [], alive: false };
  for (const [context, meta] of keys) {
    if (meta.action === "slot") {
      const it = items[meta.slot - 1];
      if (!it) {
        setTitle(context, "");
        setState(context, 0); // 비어 있음
        continue;
      }
      // 진행 중(--checklist 로 승인만 된 것)은 다른 상태로 구분한다.
      setState(context, it.inProgress ? 2 : 1);
      setTitle(context, slotLabel(it));
    } else if (meta.action === "approve" || meta.action === "dismiss") {
      // 0 은 쓰지 않는다. 스트림덱은 시선 밖에 두는 물건이라 늘 숫자가 떠 있으면
      // 변화가 묻힌다. 있음/없음은 색(state)이 훨씬 빨리 읽힌다.
      //
      // 대신 데몬이 죽은 경우는 "-" 로 구분한다. 큐가 0건인 것(정상)과 확인 자체를
      // 못 한 것(고장)이 같은 빈 키로 보이면 안 된다.
      setTitle(context, !alive ? "-" : items.length ? String(items.length) : "");
      setState(context, alive && items.length ? 1 : 0);
    } else if (meta.action === "session") {
      const s = sess.items[meta.slot - 1];
      if (!s) {
        setTitle(context, "");
        setState(context, 0);
        continue;
      }
      // 안 본 출력 > 도는 중 > 그 외. 상태는 색으로 먼저 읽힌다.
      setState(context, s.unread ? 3 : s.status === "working" ? 2 : 1);
      setTitle(context, sessionLabel(s));
    } else if (meta.action === "tts") {
      setTitle(context, alive ? "" : "-");
    }
  }
  if (!alive) {
    // 데몬이 없으면 슬롯을 비운다. 남아 있던 제목이 유령처럼 보이면 안 된다.
    // (승인/취소 키는 위에서 "-" 로 이미 구분해 뒀다)
    for (const [context, meta] of keys) {
      if (meta.action === "slot") {
        setTitle(context, "");
        setState(context, 0);
      }
    }
  }
}

ws.on("open", () => {
  send({ event: registerEvent, uuid });
  refresh();
  setInterval(refresh, 1000);
});

ws.on("message", async (raw) => {
  let msg;
  try {
    msg = JSON.parse(raw.toString());
  } catch {
    return;
  }
  const { event, context, action, payload } = msg;

  if (event === "willAppear") {
    const settings = payload?.settings ?? {};
    keys.set(context, {
      action: action?.split(".").pop() ?? "slot",
      // 좌표를 그대로 번호로 쓰면 승인/취소 키가 앞칸을 차지할 때 슬롯이 @4 부터
      // 시작해 버린다. 슬롯끼리의 순서로 매겨야 놓은 자리와 무관하게 왼쪽부터
      // @1, @2 가 된다. 설정으로 직접 지정하면 그 값이 우선.
      fixed: Number(settings.slot) || 0,
      coord: coordKey(payload),
      slot: 1,
    });
    renumber();
    refresh();
    return;
  }
  if (event === "willDisappear") {
    keys.delete(context);
    renumber();
    return;
  }
  if (event === "didReceiveSettings") {
    const meta = keys.get(context);
    if (meta) meta.slot = Number(payload?.settings?.slot) || meta.slot;
    refresh();
    return;
  }
  if (event === "keyUp") {
    const meta = keys.get(context);
    if (!meta) return;
    const at = `@${meta.slot}`;
    let res = null;
    if (meta.action === "session") {
      const s = (await getSessions()).items[meta.slot - 1];
      if (!s) return showAlert(context);
      // 이 세션이 올린 승인 대기가 있으면 그것부터. 없으면 그 터미널로 간다.
      // 물리 키 하나가 "볼 것이 있으면 처리, 없으면 데려다준다" 로 동작한다.
      const q = await knock.list();
      const idx = q.items.findIndex(
        (it) => it.source?.project && s.path?.endsWith(`/${it.source.project}`),
      );
      res = idx >= 0 ? await knock.approve(`@${idx + 1}`) : await orca.switchTo(s.id);
      if (!res) showAlert(context);
      sessionCache.at = 0; // 다음 렌더에서 즉시 반영
      return refresh();
    }
    if (meta.action === "slot") res = await knock.focus(at);
    else if (meta.action === "approve") res = await knock.approve("@1");
    else if (meta.action === "dismiss") res = await knock.dismiss("@1");
    else if (meta.action === "tts") res = await knock.toggleTts();
    // 대상이 없거나 데몬이 없으면 키에 경고를 띄운다. 아무 반응이 없으면
    // 눌리긴 한 건지 알 수 없다.
    if (!res || res.decision === "unknown") showAlert(context);
    refresh();
  }
});

ws.on("error", () => process.exit(1));
ws.on("close", () => process.exit(0));

/** 정렬용 좌표 키 — 행 우선, 그 다음 열. 없으면 맨 뒤로 보낸다. */
function coordKey(payload) {
  const c = payload?.coordinates;
  return c ? c.row * 100 + c.column : 9999;
}

/**
 * 배치된 슬롯 키들에 왼쪽 위부터 @1, @2 … 를 부여한다.
 *
 * 키를 옮기거나 지우면 자동으로 다시 매겨진다. 사람이 보는 순서와 큐의 순서가
 * 어긋나지 않는 게 이 함수의 목적이다.
 */
function renumber() {
  // 액션 종류별로 따로 매긴다. 슬롯과 세션이 한 줄에 섞여 있어도 각자 1 부터
  // 시작해야 한다 — 같이 세면 세션 키가 전부 같은 항목을 가리킨다(실측).
  for (const kind of ["slot", "session"]) {
    const group = [...keys.entries()]
      .filter(([, m]) => m.action === kind)
      .sort((a, b) => a[1].coord - b[1].coord);
    let n = 0;
    for (const [, m] of group) {
      n += 1;
      m.slot = m.fixed || n;
    }
  }
}
