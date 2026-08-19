# knock Stream Deck 플러그인

대기 중인 승인 요청을 물리 키에 띄우고 처리합니다.

## 설치

```
cp -R com.knock.controller.sdPlugin \
  ~/Library/Application\ Support/com.elgato.StreamDeck/Plugins/
```

Stream Deck 앱을 재시작하면 액션 목록에 `knock` 이 나옵니다.

## 액션

| 액션 | 하는 일 |
|------|---------|
| 대기 슬롯 | 큐의 N번째. 누르면 그 요청을 화면에 띄웁니다 |
| 승인 | 맨 앞 요청 승인 |
| 취소 | 맨 앞 요청 취소 |
| 음성 알림 | TTS on/off |
| 위로 스크롤 | 열려 있는 창 본문을 한 화면 위로 |
| 아래로 스크롤 | 열려 있는 창 본문을 한 화면 아래로 |

**스크롤 키**는 내용이 한 화면을 넘는 게이트용입니다. 보이면서 실제로 넘치는 뷰만
굴리므로, 짧은 본문에서 눌러도 아무 일도 일어나지 않습니다. 한 번에 85% 씩 움직여
문장이 두 번에 걸쳐 잘리지 않습니다.

**대기 슬롯**은 놓은 순서대로 자동으로 번호가 매겨집니다. 승인·취소 키를 앞에 두든
뒤에 두든, 슬롯끼리 왼쪽부터 1, 2, 3 입니다. 키를 옮기거나 지우면 다시 매겨집니다.

키에는 제목이 아니라 **요청 출처**가 뜹니다 — 프로젝트, 호출자(`Claude Code` /
`Codex` / `CLI`), 대기 시간. 여러 세션이 동시에 승인을 기다릴 때 구분해야 할 것이
그쪽이기 때문입니다.

## 승인과 Touch ID

승인은 이 플러그인에서 완결되지 않습니다. knock 창을 띄우고 **기존 승인 경로를
그대로 타므로** Touch ID 정책이 적용됩니다. 키를 잘못 눌러도 화면 승인과 같은
게이트를 지납니다.

물리 키 승인만 Touch ID 를 면제하려면 `~/.config/knock/config.json` 에:

```json
{ "touch_id": true, "external_skip_touch_id": true }
```

화면 승인은 지문을 요구하고, 물리 키는 즉시 통과합니다.

## 빌드

`bin/plugin.js` 는 번들 산출물입니다. 소스는 레포의 `streamdeck/` 에 있습니다.

```
cd streamdeck && bun install && bun run build
```

Stream Deck 이 번들한 Node 20 에는 WebSocket 이 내장돼 있지 않아 `ws` 를 함께
번들합니다.
