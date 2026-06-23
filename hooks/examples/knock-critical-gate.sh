#!/usr/bin/env bash
set -euo pipefail
input=$(cat)
command -v knock >/dev/null 2>&1 || exit 0
command -v jq    >/dev/null 2>&1 || exit 0
event=$(printf '%s' "$input" | jq -r '.hook_event_name // ""')
tool=$(printf  '%s' "$input" | jq -r '.tool_name // ""')
cmd=$(printf   '%s' "$input" | jq -r 'if (.tool_input|type)=="object" then (.tool_input.command // (.tool_input|tostring)) else (.tool_input|tostring) end' 2>/dev/null || echo "")
CRIT='terraform[[:space:]]+(destroy|apply)|force-delete-without-recovery|delete-secret|secretsmanager[[:space:]]+delete|schedule-key-deletion|disable-key|s3[[:space:]]+rb|s3[[:space:]]+rm[[:space:]].*--recursive|delete-objects|git[[:space:]]+reset[[:space:]]+--hard|push[[:space:]].*(--force|-f)|gh[[:space:]]+pr[[:space:]]+merge'
printf '%s' "$cmd $tool" | grep -qiE "$CRIT" || exit 0

# 명령 패턴 -> 한글 친절 요약 (대상 repo/리소스 + 위험 이유). 여러 건이면 각각 한 줄.
summary=""
addb() { summary="${summary}- ${1}"$'\n'; }
# 단순 카운트형 (대상 추출 불필요)
addcount() {
  local pat="$1" label="$2" n
  n=$(printf '%s' "$cmd" | grep -oiE "$pat" 2>/dev/null | wc -l | tr -d '[:space:]')
  if [ "${n:-0}" -gt 0 ]; then
    [ "$n" -gt 1 ] && label="$label (${n}건)"
    addb "$label"
  fi
}

# gh pr merge — 어느 repo 의 무엇(PR 번호/브랜치)을 머지하는지
while IFS= read -r ln; do
  [ -z "$ln" ] && continue
  repo=$(printf '%s' "$ln" | grep -oiE -- '--repo[ =]+[^ ]+' | head -1 | sed -E 's/.*--repo[ =]+//')
  ref=$(printf '%s' "$ln" | sed -E 's/.*[gG]h[[:space:]]+pr[[:space:]]+merge[[:space:]]+//; s/[[:space:]].*$//')
  case "$ref" in -*|'') ref='' ;; esac
  if [ -n "$ref" ]; then case "$ref" in *[!0-9]*) ref="$ref";; *) ref="#$ref";; esac; fi
  addb "GitHub PR 머지 — ${repo:-?}${ref:+ $ref} (머지 시 Actions/배포 트리거 가능)"
done < <(printf '%s\n' "$cmd" | grep -iE 'gh[[:space:]]+pr[[:space:]]+merge' || true)

# gh pr create — 어느 repo 에 어떤 제목으로 PR 생성 (머지와 함께 오는 경우 맥락 제공)
while IFS= read -r ln; do
  [ -z "$ln" ] && continue
  repo=$(printf '%s' "$ln" | grep -oiE -- '--repo[ =]+[^ ]+' | head -1 | sed -E 's/.*--repo[ =]+//')
  title=$(printf '%s' "$ln" | sed -nE 's/.*--title[ =]+"([^"]*)".*/\1/p' | head -1)
  addb "GitHub PR 생성 — ${repo:-?}${title:+ : $title}"
done < <(printf '%s\n' "$cmd" | grep -iE 'gh[[:space:]]+pr[[:space:]]+create' || true)

# Secrets Manager 삭제 — 어떤 secret
while IFS= read -r ln; do
  [ -z "$ln" ] && continue
  sid=$(printf '%s' "$ln" | grep -oiE -- '--secret-id[ =]+[^ ]+' | head -1 | sed -E 's/.*--secret-id[ =]+//')
  force=$(printf '%s' "$ln" | grep -qiE 'force-delete-without-recovery' && echo ' [즉시 영구 삭제 — 복구 불가]' || echo '')
  addb "Secrets Manager 시크릿 삭제 — ${sid:-?}${force}"
done < <(printf '%s\n' "$cmd" | grep -iE 'delete-secret|secretsmanager[[:space:]]+delete' || true)

# S3 데이터 삭제 — 어떤 버킷/경로
while IFS= read -r ln; do
  [ -z "$ln" ] && continue
  tgt=$(printf '%s' "$ln" | grep -oiE 's3://[^ ]+' | head -1)
  addb "S3 데이터 삭제 — ${tgt:-?} (데이터 손실 가능)"
done < <(printf '%s\n' "$cmd" | grep -iE 's3[[:space:]]+rb|s3[[:space:]]+rm[[:space:]].*--recursive|delete-objects' || true)

# 대상 추출이 불필요/어려운 나머지 critical
addcount 'terraform[[:space:]]+destroy'      'Terraform destroy — 인프라 리소스 삭제 (비가역, 데이터 손실 가능)'
addcount 'terraform[[:space:]]+apply'        'Terraform apply — 인프라 변경 적용'
addcount 'force-delete-without-recovery'     'KMS/시크릿 즉시 영구 삭제 (복구 창 없음 — 되돌릴 수 없음)'
addcount 'schedule-key-deletion'             'KMS 키 삭제 예약'
addcount 'disable-key'                       'KMS 키 비활성화'
addcount 'git[[:space:]]+reset[[:space:]]+--hard' 'git reset --hard — 로컬 변경 강제 폐기 (되돌릴 수 없음)'
addcount 'push[[:space:]].*(--force|-f)'     '강제 push — 원격 히스토리 덮어쓰기'
[ -z "$summary" ] && summary="- 위험 작업 (아래 명령 확인)"$'\n'

md="$(mktemp -t knock-gate.XXXXXX).md"
{
  echo "## Critical 작업 승인 필요"
  echo
  echo "**이 작업이 하는 일**"
  echo
  printf '%s' "$summary"
  echo
  echo "event: \`$event\`  tool: \`$tool\`"
  echo
  echo '```'
  printf '%s\n' "$cmd"
  echo '```'
  echo
  echo "승인 = 실행 / 닫기 = 거부"
} > "$md"
ti=""
[ "$(jq -r '.touch_id // false' "$HOME/.config/knock/config.json" 2>/dev/null)" = "true" ] && ti="--touch-id"
out=$(knock annotate "$md" --gate --json $ti 2>/dev/null || echo '{}')
rm -f "$md"
decision=$(printf '%s' "$out" | jq -r '.decision // "dismissed"' 2>/dev/null || echo "dismissed")
case "$event" in
  PreToolUse)
    if [ "$decision" = "approved" ]; then
      echo '{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"allow"}}'
    else
      echo '{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"ask","permissionDecisionReason":"knock 미승인"}}'
    fi
    ;;
  PermissionDenied)
    [ "$decision" = "approved" ] && echo '{"hookSpecificOutput":{"hookEventName":"PermissionDenied","retry":true}}'
    ;;
  *) exit 0 ;;
esac
