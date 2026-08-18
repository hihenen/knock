// orca 워크트리(= 에이전트 세션) 목록을 읽는 쪽.
//
// knock 큐가 "승인해 달라"만 알려준다면, orca 는 **세션이 무엇을 하고 있는지**를
// 알려준다. 특히 `unread` 는 "에이전트가 뭔가 말했는데 아직 안 봤다" 는 뜻이라,
// 승인 대기보다 훨씬 넓은 범위를 덮는다.
//
// orca 가 없는 환경에서는 조용히 빈 목록을 돌려준다. 이 액션만 비어 보일 뿐
// 플러그인의 나머지는 그대로 동작해야 한다.

import { execFile } from "node:child_process";

// Stream Deck 이 띄운 프로세스는 로그인 셸을 거치지 않아 PATH 가 좁다.
// which 로 찾지 말고 알려진 위치를 순서대로 본다.
const CANDIDATES = [
  "/usr/local/bin/orca",
  "/opt/homebrew/bin/orca",
  "/Applications/Orca.app/Contents/Resources/bin/orca",
];

let orcaPath = null;
let orcaMissing = false;

async function resolveOrca() {
  if (orcaPath || orcaMissing) return orcaPath;
  const fs = await import("node:fs/promises");
  for (const p of CANDIDATES) {
    try {
      await fs.access(p, (await import("node:fs")).constants.X_OK);
      orcaPath = p;
      return p;
    } catch {
      /* 다음 후보 */
    }
  }
  orcaMissing = true; // 한 번 못 찾으면 매 폴링마다 다시 뒤지지 않는다
  return null;
}

function run(args, timeoutMs = 4000) {
  return new Promise(async (resolve) => {
    const bin = await resolveOrca();
    if (!bin) return resolve(null);
    execFile(bin, args, { timeout: timeoutMs, maxBuffer: 8 * 1024 * 1024 }, (err, stdout) => {
      if (err) return resolve(null);
      try {
        resolve(JSON.parse(stdout));
      } catch {
        resolve(null);
      }
    });
  });
}

/**
 * 지금 살아 있는 세션들. 죽은 워크트리(대부분)는 걸러낸다.
 *
 * 120개 중 실제로 도는 건 두어 개다. 키에 올릴 것은 **터미널이 살아 있거나
 * 읽지 않은 출력이 있는 것**뿐이다 — 나머지를 섞으면 정작 볼 것이 묻힌다.
 */
export async function sessions() {
  const d = await run(["worktree", "ps", "--json"]);
  const all = d?.result?.worktrees;
  if (!Array.isArray(all)) return { items: [], alive: false };

  // **살아 있는 것만** 올린다. unread 만으로 포함시키면 안 된다 — 오래전에 뭔가
  // 남기고 죽은 워크트리가 13개나 되어(실측) 정작 도는 세션을 밀어낸다.
  // 안 본 출력은 살아 있는 세션 안에서 색으로 강조하는 것으로 충분하다.
  const live = all.filter((w) => !w.isArchived && (w.liveTerminalCount || 0) > 0);

  // 정렬: 안 본 것 먼저 → 도는 것 → 최근 출력 순.
  const rank = (w) => (w.unread ? 0 : w.status === "working" ? 1 : 2);
  live.sort((a, b) => rank(a) - rank(b) || (b.lastOutputAt || 0) - (a.lastOutputAt || 0));

  return {
    alive: true,
    items: live.map((w) => ({
      id: w.worktreeId,
      name: w.displayName || w.repo || "?",
      repo: w.repo,
      path: w.path,
      status: w.status, // working | active | inactive
      unread: !!w.unread,
      live: w.liveTerminalCount || 0,
      lastOutputAt: w.lastOutputAt,
    })),
  };
}

/**
 * 그 세션의 터미널 탭으로 Orca UI 를 전환한다.
 *
 * `worktree open` 같은 명령은 없다. 워크트리 -> 터미널 handle 을 먼저 찾은 뒤
 * `terminal switch` 로 간다. handle 은 실행 시마다 새로 발급되므로 캐시하지 않는다.
 */
export async function switchTo(worktreeId) {
  const d = await run(["terminal", "list"]);
  const terms = d?.result?.terminals ?? [];
  const t = terms.find((x) => x.worktreeId === worktreeId && !x.orphaned);
  if (!t?.handle) return null;
  return run(["terminal", "switch", "--terminal", t.handle], 6000);
}
