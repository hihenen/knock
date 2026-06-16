#!/usr/bin/env bash
# knock installer — downloads the latest release binary into ~/.local/bin.
#
#   curl -fsSL https://raw.githubusercontent.com/hihenen/knock/master/install.sh | bash
#
# Apple Silicon (arm64) macOS only for now.
set -euo pipefail

REPO="hihenen/knock"
INSTALL_DIR="${HOME}/.local/bin"

os="$(uname -s)"
arch="$(uname -m)"

if [ "$os" != "Darwin" ]; then
  echo "knock: 현재 macOS (Apple Silicon) 만 지원합니다. (감지: $os)" >&2
  echo "       소스 빌드: https://github.com/${REPO}#readme" >&2
  exit 1
fi

case "$arch" in
  arm64 | aarch64) binary_name="knock-macos-aarch64" ;;
  *)
    echo "knock: Apple Silicon (arm64) 만 지원합니다. (감지: $arch)" >&2
    echo "       Intel Mac 은 소스 빌드: https://github.com/${REPO}#readme" >&2
    exit 1
    ;;
esac

echo "knock: 최신 릴리스 확인 중..."
tag="$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" \
  | grep '"tag_name"' | head -1 | cut -d'"' -f4)"
if [ -z "$tag" ]; then
  echo "knock: 릴리스를 찾을 수 없습니다." >&2
  exit 1
fi

url="https://github.com/${REPO}/releases/download/${tag}/${binary_name}"
tmp="$(mktemp)"

echo "knock: ${tag} 다운로드 중..."
curl -fsSL -o "$tmp" "$url"
# Strip Gatekeeper quarantine so the unsigned binary runs without a prompt.
xattr -c "$tmp" 2>/dev/null || true
chmod +x "$tmp"

mkdir -p "$INSTALL_DIR"
mv "$tmp" "${INSTALL_DIR}/knock"
echo "knock: 설치 완료 → ${INSTALL_DIR}/knock"

case ":$PATH:" in
  *":${INSTALL_DIR}:"*) ;;
  *)
    echo ""
    echo "⚠️  ${INSTALL_DIR} 가 PATH 에 없습니다. 쉘 설정(~/.zshrc 등)에 추가하세요:"
    echo "      export PATH=\"${INSTALL_DIR}:\$PATH\""
    ;;
esac

echo ""
"${INSTALL_DIR}/knock" --version
echo ""
echo "사용법:"
echo "  knock annotate <file.md> --gate --json   # 승인 / 주석 게이트"
echo "  knock ask <questions.json>               # 객관식 질문 (AskUserQuestion 대체)"
