// knock 데몬과 이야기하는 쪽.
//
// 데몬은 유저별 Unix 도메인 소켓(/tmp/knock-{uid}.sock)에서 한 줄짜리 JSON 을
// 주고받는다. 요청 하나에 연결 하나를 쓰고 응답을 받으면 닫는다 — 게이트 등록은
// 응답까지 오래 블로킹되지만, 우리가 쓰는 제어 요청(list/focus/approve/dismiss)은
// 즉답이라 연결이 길게 남지 않는다.

import net from "node:net";
import os from "node:os";

const SOCK = `${process.env.XDG_RUNTIME_DIR || "/tmp"}/knock-${os.userInfo().uid}.sock`;

/**
 * 요청 하나를 보내고 응답 JSON 을 돌려준다.
 *
 * 데몬이 없으면 null 이다. 그 경우 **데몬을 띄우지 않는다** — 물리 키를 눌렀다고
 * 빈 창이 뜨는 건 놀라운 동작이고, 데몬이 없다는 건 대기 건도 없다는 뜻이다.
 */
export function request(payload, timeoutMs = 2000) {
  return new Promise((resolve) => {
    let done = false;
    const finish = (v) => {
      if (done) return;
      done = true;
      try {
        sock.destroy();
      } catch {
        /* 이미 닫힘 */
      }
      resolve(v);
    };

    const sock = net.createConnection(SOCK);
    const timer = setTimeout(() => finish(null), timeoutMs);
    let buf = "";

    sock.on("connect", () => sock.write(JSON.stringify(payload) + "\n"));
    sock.on("data", (chunk) => {
      buf += chunk.toString();
      const nl = buf.indexOf("\n");
      if (nl === -1) return; // 아직 한 줄이 안 됐다
      clearTimeout(timer);
      try {
        finish(JSON.parse(buf.slice(0, nl)));
      } catch {
        finish(null);
      }
    });
    // 데몬이 없거나(ENOENT) 죽었을 때. 조용히 null — 키는 그냥 아무 일도 안 한다.
    sock.on("error", () => {
      clearTimeout(timer);
      finish(null);
    });
    sock.on("close", () => {
      clearTimeout(timer);
      finish(null);
    });
  });
}

/** 대기 목록. 데몬이 없으면 빈 배열 (구분이 필요하면 `tts` 가 undefined 인지 본다). */
export async function list() {
  const r = await request({ kind: "list" });
  return { items: r?.items ?? [], tts: r?.tts, alive: !!r };
}

/** `@N` 은 큐의 N번째. 자리로 가리키는 이유는 물리 키가 id 를 모르기 때문이다. */
export const focus = (at) => request({ kind: "focus", target: at });
export const approve = (at) => request({ kind: "approve", target: at });
export const dismiss = (at) => request({ kind: "dismiss", target: at });
export const toggleTts = () => request({ kind: "tts-toggle" });
/** 열려 있는 창 본문을 한 화면 굴린다. 창이 없으면 no-window 로 돌아온다. */
export const scroll = (dir) => request({ kind: "scroll", dir });
