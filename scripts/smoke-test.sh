#!/usr/bin/env bash
# knock 기능 스모크 테스트 — 각 단계를 순서대로 실행하며 창에서 직접 확인합니다.
#   기본:  ./scripts/smoke-test.sh
#   로컬 빌드로:  KNOCK=./src-tauri/target/release/knock ./scripts/smoke-test.sh
#   특정 단계만:  ./scripts/smoke-test.sh 3   (3번만 실행)

BIN="${KNOCK:-knock}"
ONLY="${1:-}"
run() { [ -z "$ONLY" ] || [ "$ONLY" = "$1" ]; }

echo "== knock $("$BIN" --version)  (BIN=$BIN)"
echo

# ── 1. annotate — 승인 / 변경요청 / 취소 + 본문 링크 ─────────────────
if run 1; then
  cat > /tmp/k-annotate.md <<'EOF'
# 배포 승인 (테스트)

prd 에 X 를 배포합니다.

| 항목 | 값 |
|---|---|
| 영향 | A, B |
| 롤백 | revert |

참조: [knock repo](https://github.com/hihenen/knock) — 링크 클릭 시 외부 브라우저로 열려야 함
EOF
  echo "[1] annotate --gate  → 승인 / 변경요청(피드백) / 취소, 본문 링크 클릭 확인"
  "$BIN" annotate /tmp/k-annotate.md --gate --json
  echo "    출력 ↑ (approved / annotated+feedback / dismissed)"
  echo
fi

# ── 2. action inbox — 승인 시 브라우저로 점프 ───────────────────────
if run 2; then
  echo "[2] annotate --action-url  → 승인 누르면 브라우저가 releases 로 점프"
  "$BIN" annotate /tmp/k-annotate.md --gate \
    --action-url "https://github.com/hihenen/knock/releases"
  echo
fi

# ── 3. ask — context(맥락) + 체크박스(여러 개 선택 + 기타) ──────────
if run 3; then
  cat > /tmp/k-ask.json <<'EOF'
{
  "context": "## 배경\n\n| 옵션 | 장점 | 단점 |\n|---|---|---|\n| A | 빠름 | 위험 |\n| B | 안전 | 느림 |\n\n**결론**: 상황에 따라 1~2개 + 기타 메모로 답해보세요.",
  "questions": [
    { "header": "방향", "question": "어느 방향으로 갈까?", "options": [
      { "label": "A안", "description": "빠름" },
      { "label": "B안", "description": "안전" },
      { "label": "C안", "description": "보류" }
    ]}
  ]
}
EOF
  echo "[3] ask  → 질문 위에 맥락 렌더, 체크박스로 1·2 여러 개 선택 + 기타 메모"
  "$BIN" ask /tmp/k-ask.json
  echo "    출력 ↑ {\"answers\":{\"방향\":[...]}} (항상 배열)"
  echo
fi

# ── 4. 멀티세션 큐 — 동시 호출 2개가 한 창에 큐로 ───────────────────
if run 4; then
  echo "[4] 멀티세션 큐  → 2개 동시 호출이 한 창에 모이고 Dock 뱃지 2"
  echo "# 세션 A 승인" > /tmp/k-a.md
  echo "# 세션 B 승인" > /tmp/k-b.md
  "$BIN" annotate /tmp/k-a.md --gate & sleep 1.5
  "$BIN" annotate /tmp/k-b.md --gate &
  wait
  echo
fi

# ── 5. settings — Touch ID 토글 / 버그 신고 / 릴리스 노트 / 버전 ─────
if run 5; then
  echo "[5] settings  → Touch ID 토글, 하단 버그 신고·릴리스 노트 링크 + 버전"
  "$BIN" settings
  echo
fi

# ── 6. daemon — 상주 상태 ───────────────────────────────────────────
if run 6; then
  echo "[6] daemon status  (install/uninstall 로 로그인 상주 토글)"
  "$BIN" daemon status
  echo
fi

echo "== done"
