use regex::{Captures, Regex};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};
use std::{io, thread};
use tauri::{Emitter, Manager};
use tauri_plugin_notification::NotificationExt;
use tungstenite::{stream::MaybeTlsStream, Message, WebSocket};
use url::Url;

const DEFAULT_HOSTS: &[&str] = &["fnfcorp.scalr.io"];
const MAX_BACKOFF_SECS: u64 = 60;
const SOCKET_POLL: Duration = Duration::from_secs(1);
const API_TIMEOUT: Duration = Duration::from_secs(15);
const SOCKET_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// 클라이언트가 WebSocket ping 을 보내는 간격.
const PING_INTERVAL: Duration = Duration::from_secs(30);
/// 이 시간 동안 어떤 프레임도 없으면 끊긴 소켓(half-open)으로 본다.
const LIVENESS_TIMEOUT: Duration = Duration::from_secs(90);
/// 토큰 읽기 실패 뒤 다시 읽기까지의 최소 간격 (1분 → 최대 15분). 키체인 창 연타 방지.
const TOKEN_RETRY_BASE_SECS: u64 = 60;
const TOKEN_RETRY_MAX_SECS: u64 = 900;
/// 끊김이 이만큼 이어지면 macOS 알림을 한 번 보낸다.
const DISCONNECT_ALERT_AFTER: Duration = Duration::from_secs(300);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(super) struct SlackConfig {
    pub enabled: bool,
    pub owner_user_id: String,
    pub remote_action_hosts: Vec<String>,
    /// true 면 `--gate` 게이트가 `--remote-approve` 없이도 Slack 대상이 된다 (`--local-only` 로 제외).
    pub remote_default: bool,
}

impl Default for SlackConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            owner_user_id: String::new(),
            remote_action_hosts: DEFAULT_HOSTS.iter().map(|host| (*host).into()).collect(),
            remote_default: false,
        }
    }
}

pub(super) fn config_from_value(value: &Value) -> SlackConfig {
    serde_json::from_value(value["slack"].clone()).unwrap_or_default()
}

pub(super) fn config_payload(value: &Value) -> Value {
    serde_json::to_value(config_from_value(value)).expect("SlackConfig is serializable")
}

pub(super) fn normalize_hosts(hosts: Vec<String>) -> Result<Vec<String>, String> {
    let mut normalized = Vec::new();
    for host in hosts {
        let host = host.trim().to_ascii_lowercase();
        let (wildcard, value) = host
            .strip_prefix("*.")
            .map(|value| (true, value))
            .unwrap_or((false, host.as_str()));
        if value.is_empty()
            || value
                .chars()
                .any(|c| matches!(c, '/' | ':' | '@' | '?' | '#'))
            || url::Host::parse(value).is_err()
        {
            return Err("원격 승인 호스트는 도메인 이름 또는 *.도메인 형식이어야 합니다".into());
        }
        let value = if wildcard {
            format!("*.{value}")
        } else {
            value.to_string()
        };
        if !normalized.contains(&value) {
            normalized.push(value);
        }
    }
    Ok(normalized)
}

pub(super) fn is_remote_target(
    config: &SlackConfig,
    gate: bool,
    explicit: bool,
    local_only: bool,
    touch_id: bool,
    ask: bool,
    action_url: Option<&str>,
) -> bool {
    // --local-only 는 --remote-approve·remote_default·허용 호스트 어느 것보다 우선한다.
    if !config.enabled || !gate || touch_id || ask || local_only {
        return false;
    }
    if explicit || config.remote_default {
        return true;
    }
    let Some(host) = action_url
        .and_then(|url| Url::parse(url).ok())
        .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
    else {
        return false;
    };
    config.remote_action_hosts.iter().any(|pattern| {
        if let Some(suffix) = pattern.strip_prefix("*.") {
            host.ends_with(&format!(".{suffix}"))
        } else {
            host == pattern.to_ascii_lowercase()
        }
    })
}

static MASKING_PATTERNS: OnceLock<[Regex; 5]> = OnceLock::new();

/// 형태가 뚜렷한 토큰·자격증명 (패턴, 치환). 위 기본 5종 뒤에 적용한다.
static EXTRA_MASKING_PATTERNS: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();

fn extra_masking_patterns() -> &'static [(Regex, &'static str)] {
    EXTRA_MASKING_PATTERNS.get_or_init(|| {
        [
            // AWS secret key 등 이름 중간·끝에 secret/access key/api key 가 낀 변수 (AWS_SECRET_ACCESS_KEY=...)
            (r#"(?i)([A-Za-z0-9_.-]*(?:secret|passwd|api[_-]?key|access[_-]?key|private[_-]?key|credential)[A-Za-z0-9_.-]*["']?\s*[:=]\s*)("(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])*'|[^\s,;&"']+)"#, "${1}****"),
            // CLI 플래그: --secret-access-key VALUE, --password=VALUE
            (r"(?i)(--[a-z-]*(?:secret|token|password|api-key|access-key)[a-z-]*[ =]+)([^\s]+)", "${1}****"),
            // Authorization 헤더
            (r"(?i)\b(Bearer|Basic)\s+[A-Za-z0-9._~+/=-]{8,}", "${1} ****"),
            // OpenAI / Anthropic 키
            (r"\bsk-(?:ant-)?[A-Za-z0-9_-]{16,}", "****"),
            // GitHub OAuth·서버·사용자·갱신 토큰
            (r"\bgh[ousr]_[A-Za-z0-9]{20,}\b", "****"),
            // JWT
            (r"\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]*", "****"),
            // URL 속 비밀번호: scheme://user:PASS@host
            (r"(?i)\b([a-z][a-z0-9+.-]*://[^\s:/@]+:)[^\s@/]+(@)", "${1}****${2}"),
        ]
        .into_iter()
        .map(|(pattern, replacement)| (Regex::new(pattern).unwrap(), replacement))
        .collect()
    })
}

pub(super) fn mask_secrets(text: &str) -> String {
    let patterns = MASKING_PATTERNS.get_or_init(|| {
        [
            Regex::new(
                r"-----BEGIN [^\r\n]*PRIVATE KEY-----[\s\S]*?-----END [^\r\n]*PRIVATE KEY-----",
            ).unwrap(),
            Regex::new(r"\b(?:AKIA|ASIA)[A-Z0-9]{16}\b").unwrap(),
            Regex::new(r"\b(?:xox[abpr]-|xapp-)[A-Za-z0-9-]+\b").unwrap(),
            Regex::new(r"\b(?:ghp_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,})\b").unwrap(),
            Regex::new(r#"(?i)(["']?(?:password|secret|token)["']?\s*[:=]\s*)("(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])*'|[^\s,;&]+)"#).unwrap(),
        ]
    });
    let mut masked = text.to_string();
    for pattern in &patterns[..4] {
        masked = pattern.replace_all(&masked, "****").into_owned();
    }
    let masked = patterns[4]
        .replace_all(&masked, |captures: &Captures<'_>| {
            let value = captures.get(2).map(|m| m.as_str()).unwrap_or_default();
            let quote = if value.starts_with('"') && value.ends_with('"') {
                "\""
            } else if value.starts_with('\'') && value.ends_with('\'') {
                "'"
            } else {
                ""
            };
            format!("{0}{1}****{1}", &captures[1], quote)
        })
        .into_owned();
    extra_masking_patterns()
        .iter()
        .fold(masked, |text, (pattern, replacement)| {
            pattern.replace_all(&text, *replacement).into_owned()
        })
}

fn truncate_body(text: &str, limit: usize) -> String {
    let masked = mask_secrets(text);
    let mut chars = masked.chars();
    let body: String = chars.by_ref().take(limit).collect();
    if chars.next().is_some() {
        format!("{body}…(맥에서 전체 보기)")
    } else {
        body
    }
}

const SLACK_SECTION_TEXT_LIMIT: usize = 3000;
const SLACK_BLOCK_LIMIT: usize = 50;
const SLACK_BODY_LIMIT: usize = 1500;

#[derive(Default)]
struct ExtractedBody {
    requester: Option<String>,
    requested_at: Option<String>,
    execution: Option<String>,
    ai_judgment: Option<String>,
    body: String,
}

fn metadata_value(line: &str, emoji: &str, label: &str) -> Option<String> {
    let tail = line
        .trim()
        .strip_prefix(emoji)?
        .trim()
        .strip_prefix(label)?
        .trim_start();
    Some(tail.trim_start_matches([':', '：', '-']).trim().into())
}

fn markdown_heading(line: &str) -> Option<(usize, &str)> {
    let line = line.trim_start();
    let level = line.bytes().take_while(|byte| *byte == b'#').count();
    let title = line.get(level..)?;
    ((1..=6).contains(&level) && title.chars().next().is_some_and(char::is_whitespace))
        .then_some((level, title.trim().trim_end_matches('#').trim()))
}

fn unwrap_code_fence(value: &str) -> String {
    let mut lines = value.lines();
    let Some(open) = lines.next().and_then(fence_marker) else {
        return value.trim().into();
    };
    let mut rest: Vec<_> = lines.collect();
    if rest.last().is_some_and(|line| is_fence_close(line, open)) {
        rest.pop();
    }
    rest.join("\n").trim().into()
}

fn unwrap_inline_code(value: &str) -> String {
    static CODE: OnceLock<Regex> = OnceLock::new();
    CODE.get_or_init(|| Regex::new(r"`([^`]+)`").unwrap())
        .replace_all(value.trim(), "$1")
        .trim()
        .into()
}

fn extract_body_sections(masked_body: &str) -> ExtractedBody {
    let lines: Vec<&str> = masked_body.lines().collect();
    let mut result = ExtractedBody::default();
    let mut execution_lines = Vec::new();
    let mut index = 0;
    let mut pending_execution = false;
    while index < lines.len() {
        let line = lines[index];
        if line.trim().is_empty() {
            index += 1;
            continue;
        }
        if let Some((emoji, value)) = [("👤", "요청자"), ("⏱", "요청시각"), ("▶️", "승인 시 실행")]
            .into_iter()
            .find_map(|(emoji, label)| {
                metadata_value(line, emoji, label).map(|value| (emoji, value))
            })
        {
            match emoji {
                "👤" => result.requester = Some(value),
                "⏱" => result.requested_at = Some(value),
                _ => {
                    pending_execution = value.is_empty();
                    if !value.is_empty() {
                        execution_lines.push(value);
                    }
                }
            }
            index += 1;
            continue;
        }
        break;
    }
    if pending_execution && index < lines.len() && !lines[index].trim().is_empty() {
        execution_lines.push(lines[index].to_string());
        index += 1;
        if let Some(fence) = fence_marker(&execution_lines[0]) {
            while index < lines.len() {
                let line = lines[index];
                execution_lines.push(line.to_string());
                index += 1;
                if is_fence_close(line, fence) {
                    break;
                }
            }
        } else {
            while index < lines.len() && !lines[index].trim().is_empty() {
                execution_lines.push(lines[index].to_string());
                index += 1;
            }
        }
    }
    if !execution_lines.is_empty() {
        let execution = execution_lines.join("\n");
        let fenced = execution
            .lines()
            .next()
            .is_some_and(|line| fence_marker(line).is_some());
        let execution = unwrap_code_fence(&execution);
        result.execution = Some(if fenced {
            execution
        } else {
            unwrap_inline_code(&execution)
        });
    }

    let mut remaining = Vec::new();
    let mut ai_lines = Vec::new();
    let mut ai_level = None;
    let mut fence = None;
    for line in &lines[index..] {
        let marker = fence_marker(line);
        if fence.is_none() {
            if ai_level
                .is_some_and(|level| markdown_heading(line).is_some_and(|(next, _)| next <= level))
            {
                ai_level = None;
            }
            if ai_level.is_none() {
                ai_level = markdown_heading(line)
                    .and_then(|(level, title)| (title == "🤖 AI 판단").then_some(level));
                if ai_level.is_some() {
                    continue;
                }
            }
        }
        let target = if ai_level.is_some() {
            &mut ai_lines
        } else {
            &mut remaining
        };
        target.push(*line);
        if let Some(open) = fence {
            if marker.is_some_and(|current| is_fence_close(line, open) && current.0 == open.0) {
                fence = None;
            }
        } else if let Some(marker) = marker {
            fence = Some(marker);
        }
    }
    if !ai_lines.is_empty() {
        result.ai_judgment = Some(ai_lines.join("\n").trim().to_string());
    }
    result.body = remaining.join("\n").trim().to_string();
    result
}

fn escape_mrkdwn_text(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn format_inline_mrkdwn(text: &str) -> String {
    static CODE: OnceLock<Regex> = OnceLock::new();
    static LINKS: OnceLock<Regex> = OnceLock::new();
    static BOLD: OnceLock<Regex> = OnceLock::new();
    static UNDERSCORE_BOLD: OnceLock<Regex> = OnceLock::new();
    let code = CODE.get_or_init(|| Regex::new(r"`([^`]+)`").unwrap());
    let links = LINKS.get_or_init(|| Regex::new(r#"\[([^\]]+)\]\(([^)]+)\)"#).unwrap());
    let bold = BOLD.get_or_init(|| Regex::new(r"\*\*(.+?)\*\*").unwrap());
    let underscore_bold = UNDERSCORE_BOLD.get_or_init(|| Regex::new(r"__(.+?)__").unwrap());
    let mut replacements = Vec::new();
    let protected = code.replace_all(text, |caps: &Captures<'_>| {
        replacements.push(format!("`{}`", escape_mrkdwn_text(&caps[1])));
        format!("\u{e000}{}\u{e001}", replacements.len() - 1)
    });
    let protected = links.replace_all(&protected, |caps: &Captures<'_>| {
        let label = format_plain_inline(&caps[1], bold, underscore_bold);
        replacements.push(format!("<{}|{}>", escape_mrkdwn_text(&caps[2]), label));
        format!("\u{e000}{}\u{e001}", replacements.len() - 1)
    });
    let mut result = format_plain_inline(&protected, bold, underscore_bold);
    for (index, replacement) in replacements.iter().enumerate().rev() {
        result = result.replace(&format!("\u{e000}{index}\u{e001}"), replacement);
    }
    result
}

fn format_plain_inline(text: &str, bold: &Regex, underscore_bold: &Regex) -> String {
    let escaped = escape_mrkdwn_text(text);
    let text = bold.replace_all(&escaped, "*$1*");
    let text = underscore_bold.replace_all(&text, "_${1}_");
    insert_emphasis_boundaries(&text)
}

fn insert_emphasis_boundaries(text: &str) -> String {
    static MARKER: OnceLock<Regex> = OnceLock::new();
    let marker = MARKER.get_or_init(|| {
        Regex::new(r"(?:\*[^*\s](?:[^*]*?[^*\s])?\*|_[^_\s](?:[^_]*?[^_\s])?_|~~[^~\s](?:[^~]*?[^~\s])?~~|~[^~\s](?:[^~]*?[^~\s])?~)").unwrap()
    });
    let is_word = |ch: char| ch.is_alphanumeric() || ch == '_';
    let mut boundaries = Vec::new();
    for found in marker.find_iter(text) {
        if text[..found.start()]
            .chars()
            .next_back()
            .is_some_and(is_word)
        {
            boundaries.push(found.start());
        }
        if text[found.end()..].chars().next().is_some_and(is_word) {
            boundaries.push(found.end());
        }
    }
    boundaries.sort_unstable();
    boundaries.dedup();
    let mut output = String::with_capacity(text.len() + boundaries.len() * 3);
    let mut start = 0;
    for boundary in boundaries {
        output.push_str(&text[start..boundary]);
        output.push('\u{200b}');
        start = boundary;
    }
    output.push_str(&text[start..]);
    output
}

fn is_table_separator(line: &str) -> bool {
    let line = line.trim().trim_matches('|');
    !line.is_empty()
        && line.split('|').all(|cell| {
            let cell = cell.trim().trim_matches(':');
            cell.len() >= 3 && cell.chars().all(|ch| ch == '-')
        })
}

fn fence_marker(line: &str) -> Option<(char, usize)> {
    let line = line.trim_start();
    let ch = line.chars().next()?;
    let count = line.chars().take_while(|current| *current == ch).count();
    (matches!(ch, '`' | '~') && count >= 3).then_some((ch, count))
}

fn is_fence_close(line: &str, open: (char, usize)) -> bool {
    let line = line.trim_start();
    fence_marker(line).is_some_and(|(ch, count)| {
        ch == open.0 && count >= open.1 && line[count..].trim().is_empty()
    })
}

fn markdown_to_mrkdwn(markdown: &str) -> String {
    let lines: Vec<&str> = markdown.lines().collect();
    let mut output = String::new();
    let mut index = 0;
    let mut fence = None;

    while index < lines.len() {
        let line = lines[index];
        if let Some(marker) = fence_marker(line) {
            if fence.is_some_and(|open| is_fence_close(line, open)) {
                output.push_str("```\n");
                fence = None;
            } else if fence.is_none() {
                output.push_str("```\n");
                fence = Some(marker);
            } else {
                output.push_str(&format!("{}\n", escape_mrkdwn_text(line)));
            }
            index += 1;
            continue;
        }
        if fence.is_some() {
            output.push_str(&format!("{}\n", escape_mrkdwn_text(line)));
            index += 1;
            continue;
        }
        if index + 1 < lines.len() && line.contains('|') && is_table_separator(lines[index + 1]) {
            output.push_str("```\n");
            while index < lines.len()
                && !lines[index].trim().is_empty()
                && lines[index].contains('|')
            {
                output.push_str(&format!("{}\n", escape_mrkdwn_text(lines[index])));
                index += 1;
            }
            output.push_str("```\n");
            continue;
        }

        let converted = markdown_heading(line)
            .map(|(_, title)| format!("*{}*", format_inline_mrkdwn(title)))
            .unwrap_or_else(|| {
                let indentation = &line[..line.len() - line.trim_start().len()];
                let item = &line[indentation.len()..];
                if let Some(item) = item.strip_prefix("- ").or_else(|| item.strip_prefix("* ")) {
                    format!("{indentation}• {}", format_inline_mrkdwn(item))
                } else {
                    format_inline_mrkdwn(line)
                }
            });
        output.push_str(&format!("{converted}\n"));
        index += 1;
    }
    if fence.is_some() {
        output.push_str("```\n");
    }
    output.trim_end().to_string()
}

fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

fn truncate_slack_text(text: &str, limit: usize) -> String {
    if utf16_len(text) <= limit {
        return text.to_string();
    }
    let suffix = "…(맥에서 전체 보기)";
    let budget = limit.saturating_sub(utf16_len(suffix) + 4);
    let mut output = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let units = ch.len_utf16();
        if used + units > budget {
            break;
        }
        output.push(ch);
        used += units;
    }
    if let Some(index) = output
        .rfind('&')
        .filter(|&index| !output[index..].contains(';'))
    {
        output.truncate(index);
    }
    if output.matches("```").count() % 2 == 1 {
        output.push_str("\n```");
    }
    output.push_str(suffix);
    output
}

fn add_section_blocks(blocks: &mut Vec<Value>, text: &str) {
    if blocks.len() < SLACK_BLOCK_LIMIT - 1 {
        let text = truncate_slack_text(text, SLACK_SECTION_TEXT_LIMIT);
        blocks.push(json!({ "type": "section", "text": { "type": "mrkdwn", "text": text } }));
    }
}

fn build_message_blocks(
    id: &str,
    title: &str,
    body: &str,
    action_url: Option<&str>,
) -> (String, Vec<Value>) {
    let title: String = mask_secrets(title).chars().take(150).collect();
    let title = if title.trim().is_empty() {
        "Knock 승인 요청".to_string()
    } else {
        title
    };
    let extracted = extract_body_sections(&mask_secrets(body));
    let empty = extracted.body.trim().is_empty()
        && [
            &extracted.execution,
            &extracted.ai_judgment,
            &extracted.requester,
            &extracted.requested_at,
        ]
        .iter()
        .all(|value| value.is_none());
    let body = if empty {
        "(본문 없음)".to_string()
    } else {
        truncate_body(&extracted.body, SLACK_BODY_LIMIT)
    };
    let bounded = |value: Option<&str>| truncate_body(value.unwrap_or_default(), SLACK_BODY_LIMIT);
    let execution = bounded(extracted.execution.as_deref());
    let ai_judgment = bounded(extracted.ai_judgment.as_deref());
    let action_url = action_url.map(mask_secrets);

    let mut blocks = vec![json!({
        "type": "section",
        "text": { "type": "mrkdwn", "text": format!("⚠️ *{}* 승인이 필요합니다", escape_mrkdwn_text(&title)) }
    })];
    let context = [
        ("👤", extracted.requester.as_deref()),
        ("⏱", extracted.requested_at.as_deref()),
    ]
    .into_iter()
    .filter_map(|(label, value)| {
        value
            .filter(|value| !value.is_empty())
            .map(|value| format!("{label} {}", escape_mrkdwn_text(value)))
    })
    .collect::<Vec<_>>()
    .join(" · ");
    let context = truncate_slack_text(&context, SLACK_SECTION_TEXT_LIMIT);
    if !context.is_empty() {
        blocks.push(json!({
            "type": "context",
            "elements": [{ "type": "mrkdwn", "text": context }]
        }));
    }

    let execution_value = if execution.trim().is_empty() {
        action_url
            .as_deref()
            .map(|url| {
                let url =
                    truncate_slack_text(&escape_mrkdwn_text(url), SLACK_SECTION_TEXT_LIMIT - 40);
                format!("<{url}|작업 열기>")
            })
            .unwrap_or_default()
    } else {
        format!("```\n{}\n```", escape_mrkdwn_text(&execution))
    };
    let execution_section = format!("▶️ 승인 시 실행\n{execution_value}");
    add_section_blocks(&mut blocks, execution_section.trim_end());
    let formatted_body = markdown_to_mrkdwn(&body);
    if !formatted_body.trim().is_empty() {
        add_section_blocks(&mut blocks, &formatted_body);
    }
    let ai = if ai_judgment.trim().is_empty() {
        String::new()
    } else {
        format!("🤖 *AI 판단*\n{}", markdown_to_mrkdwn(&ai_judgment))
    };
    if !ai.is_empty() {
        add_section_blocks(&mut blocks, &ai);
    }
    blocks.push(json!({ "type": "actions", "elements": [
        { "type": "button", "action_id": "knock_approve", "text": { "type": "plain_text", "text": "승인", "emoji": true }, "style": "primary", "value": id },
        { "type": "button", "action_id": "knock_dismiss", "text": { "type": "plain_text", "text": "취소", "emoji": true }, "style": "danger", "value": id },
        { "type": "button", "action_id": "knock_approve_all", "text": { "type": "plain_text", "text": "모두 승인", "emoji": true }, "value": id }
    ] }));
    let fallback = format!("{title}\n{context}\n{execution_section}\n{formatted_body}\n{ai}");
    (fallback, blocks)
}

/// 선택지가 있으면 마지막 승인/취소 actions 블록을 선택지 버튼 + 취소로 바꾼다.
fn build_post_blocks(
    id: &str,
    title: &str,
    body: &str,
    action_url: Option<&str>,
    options: &[String],
) -> (String, Vec<Value>) {
    let (fallback, mut blocks) = build_message_blocks(id, title, body, action_url);
    if options.is_empty() {
        return (fallback, blocks);
    }
    if blocks
        .last()
        .is_some_and(|block| block["type"] == "actions")
    {
        blocks.pop();
    }
    let mut elements: Vec<Value> = options
        .iter()
        .enumerate()
        .map(|(index, label)| {
            let text: String = mask_secrets(label).chars().take(70).collect();
            json!({ "type": "button", "action_id": format!("knock_ask_opt_{index}"),
                "text": { "type": "plain_text", "text": text, "emoji": true },
                "value": format!("{id}|{index}") })
        })
        .collect();
    elements.push(json!({ "type": "button", "action_id": "knock_dismiss",
        "text": { "type": "plain_text", "text": "취소", "emoji": true }, "style": "danger", "value": id }));
    blocks.push(json!({ "type": "actions", "elements": elements }));
    (fallback, blocks)
}

fn build_post_view(
    id: &str,
    title: &str,
    body: &str,
    action_url: Option<&str>,
    ask: Option<&AskSpec>,
) -> (String, Vec<Value>) {
    match ask {
        Some(spec) => build_ask_blocks(id, title, spec, &[], None),
        None => build_post_blocks(id, title, body, action_url, &[]),
    }
}

/// 질문 하나의 Slack 표현: 답 키(맥 창 `keyFor` 와 같다), 본문 마크다운, 복수 선택 여부, 선택지 라벨.
#[derive(Clone, Debug)]
pub(super) struct AskQuestion {
    key: String,
    body: String,
    multi: bool,
    labels: Vec<String>,
}

#[derive(Clone, Debug)]
pub(super) struct AskSpec {
    questions: Vec<AskQuestion>,
}

/// 질문 하나에 지금까지 고른 것: 선택지 인덱스(오름차순)와 "기타" 입력 원문.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct AskPick {
    sel: Vec<usize>,
    other: Option<String>,
}

const ASK_MAX_QUESTIONS: usize = 4;
const ASK_MAX_BUTTON_OPTIONS: usize = 24;
const ASK_MAX_CHECKBOX_OPTIONS: usize = 10;

impl AskSpec {
    fn any_multi(&self) -> bool {
        self.questions.iter().any(|question| question.multi)
    }
}

/// 맥 창 `labelsFor` 와 같은 순서: 고른 선택지 라벨들, 그 뒤에 기타 입력.
fn ask_labels(question: &AskQuestion, pick: &AskPick) -> Vec<String> {
    let mut out: Vec<String> = pick
        .sel
        .iter()
        .filter_map(|&index| question.labels.get(index).cloned())
        .collect();
    if let Some(other) = pick
        .other
        .as_deref()
        .map(str::trim)
        .filter(|o| !o.is_empty())
    {
        out.push(other.to_string());
    }
    out
}

/// Slack 으로 보낼 수 있는 `ask` 인지 판정한다: 질문 1~4개, 질문마다 선택지 1~24개
/// (복수 선택은 체크박스 한도 10개). 한도를 넘으면 None(맥 창 전용).
pub(super) fn ask_remote_spec(inner: &Value) -> Option<AskSpec> {
    let questions = inner.get("questions")?.get("questions")?.as_array()?;
    if questions.is_empty() || questions.len() > ASK_MAX_QUESTIONS {
        return None;
    }
    let mut out = Vec::with_capacity(questions.len());
    for (qi, question) in questions.iter().enumerate() {
        let multi = question
            .get("multiSelect")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let options = question.get("options")?.as_array()?;
        let limit = if multi {
            ASK_MAX_CHECKBOX_OPTIONS
        } else {
            ASK_MAX_BUTTON_OPTIONS
        };
        if options.is_empty() || options.len() > limit {
            return None;
        }
        let labels: Vec<String> = options
            .iter()
            .filter_map(|option| option.get("label").and_then(Value::as_str))
            .map(str::to_string)
            .collect();
        if labels.len() != options.len() || labels.iter().any(|label| label.trim().is_empty()) {
            return None;
        }
        let mut body = String::new();
        if let Some(text) = question.get("question").and_then(Value::as_str) {
            body.push_str(text);
            body.push_str("\n\n");
        }
        for option in options {
            let label = option
                .get("label")
                .and_then(Value::as_str)
                .unwrap_or_default();
            match option
                .get("description")
                .and_then(Value::as_str)
                .filter(|d| !d.is_empty())
            {
                Some(description) => body.push_str(&format!("- **{label}** — {description}\n")),
                None => body.push_str(&format!("- **{label}**\n")),
            }
        }
        let text_of = |field: &str| {
            question
                .get(field)
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
        };
        let key = text_of("header")
            .or_else(|| text_of("question"))
            .map(str::to_string)
            .unwrap_or_else(|| format!("q{qi}"));
        out.push(AskQuestion {
            key,
            body,
            multi,
            labels,
        });
    }
    Some(AskSpec { questions: out })
}

fn ask_other_button(action_id: String, value: String, text: &str) -> Value {
    json!({ "type": "button", "action_id": action_id, "value": value,
        "text": { "type": "plain_text", "text": text, "emoji": true } })
}

fn ask_plain(text: &str, limit: usize) -> String {
    mask_secrets(text).chars().take(limit).collect()
}

/// `ask` 메시지 블록. 질문 1개·단일 선택은 기존 버튼 한 줄(+기타), 그 밖은 질문마다 섹션 + 선택 UI,
/// 끝에 [제출](복수 선택이 있을 때)·[취소]. `picks` 의 선택은 ✓/체크로 그린다.
fn build_ask_blocks(
    id: &str,
    title: &str,
    spec: &AskSpec,
    picks: &[AskPick],
    notice: Option<&str>,
) -> (String, Vec<Value>) {
    let hint = "기타 답은 `기타…` 버튼으로 입력합니다";
    let (fallback, mut blocks) = match spec.questions.as_slice() {
        [question] if !question.multi => {
            let (fallback, mut blocks) =
                build_post_blocks(id, title, &question.body, None, &question.labels);
            if let Some(elements) = blocks
                .last_mut()
                .and_then(|block| block["elements"].as_array_mut())
            {
                let at = elements.len().saturating_sub(1);
                elements.insert(
                    at,
                    ask_other_button("knock_ask_other_0".into(), format!("{id}|0"), "기타…"),
                );
            }
            (fallback, blocks)
        }
        questions => {
            let count = questions.len();
            let intro = if spec.any_multi() {
                format!("질문 {count}개 — 복수 선택 질문은 체크한 뒤 [제출]을 누르세요.")
            } else {
                format!("질문 {count}개 — 질문마다 하나씩 고르세요.")
            };
            let (fallback, mut blocks) = build_message_blocks(id, title, &intro, None);
            blocks.pop();
            let empty = AskPick::default();
            for (qi, question) in questions.iter().enumerate() {
                let pick = picks.get(qi).unwrap_or(&empty);
                let section = format!(
                    "*{}/{count}* {}",
                    qi + 1,
                    markdown_to_mrkdwn(&mask_secrets(&question.body))
                );
                add_section_blocks(&mut blocks, &section);
                let mut elements = Vec::new();
                if question.multi {
                    let options: Vec<Value> = question
                        .labels
                        .iter()
                        .enumerate()
                        .map(|(oi, label)| {
                            json!({ "text": { "type": "plain_text", "text": ask_plain(label, 75) },
                                "value": format!("{id}|{qi}|{oi}") })
                        })
                        .collect();
                    let mut checkboxes = json!({ "type": "checkboxes",
                        "action_id": format!("knock_ask_chk_{qi}"), "options": options });
                    let initial: Vec<Value> = pick
                        .sel
                        .iter()
                        .filter_map(|&oi| checkboxes["options"].get(oi).cloned())
                        .collect();
                    if !initial.is_empty() {
                        checkboxes["initial_options"] = Value::Array(initial);
                    }
                    elements.push(checkboxes);
                } else {
                    for (oi, label) in question.labels.iter().enumerate() {
                        let chosen = pick.sel.contains(&oi);
                        let text = ask_plain(label, 68);
                        let mut button = json!({ "type": "button",
                            "action_id": format!("knock_ask_q{qi}_o{oi}"),
                            "text": { "type": "plain_text", "emoji": true,
                                "text": if chosen { format!("✓ {text}") } else { text } },
                            "value": format!("{id}|{qi}|{oi}") });
                        if chosen {
                            button["style"] = json!("primary");
                        }
                        elements.push(button);
                    }
                }
                elements.push(ask_other_button(
                    format!("knock_ask_other_{qi}"),
                    format!("{id}|{qi}"),
                    if pick.other.is_some() {
                        "✓ 기타…"
                    } else {
                        "기타…"
                    },
                ));
                blocks.push(json!({ "type": "actions", "block_id": format!("{qi}|{id}"),
                    "elements": elements }));
                if let Some(other) = pick.other.as_deref() {
                    let shown = truncate_slack_text(
                        &escape_mrkdwn_text(&mask_secrets(other)),
                        SLACK_SECTION_TEXT_LIMIT,
                    );
                    blocks.push(json!({ "type": "context", "elements": [
                        { "type": "mrkdwn", "text": format!("✍️ 기타: {shown}") }
                    ] }));
                }
            }
            let mut last = Vec::new();
            if spec.any_multi() {
                last.push(json!({ "type": "button", "action_id": "knock_ask_submit", "value": id,
                    "text": { "type": "plain_text", "text": "제출", "emoji": true }, "style": "primary" }));
            }
            last.push(json!({ "type": "button", "action_id": "knock_dismiss", "value": id,
                "text": { "type": "plain_text", "text": "취소", "emoji": true }, "style": "danger" }));
            blocks.push(json!({ "type": "actions", "elements": last }));
            (fallback, blocks)
        }
    };
    let mut notes = vec![hint.to_string()];
    notes.extend(notice.map(str::to_string));
    let at = blocks.len().saturating_sub(1);
    for (offset, note) in notes.into_iter().enumerate() {
        blocks.insert(
            at + offset,
            json!({ "type": "context", "elements": [{ "type": "mrkdwn", "text": note }] }),
        );
    }
    (fallback, blocks)
}

fn resolution_parts(decision: &str) -> (&'static str, &'static str, &'static str) {
    match decision {
        "approved" => ("✅", "승인", "승인됨"),
        "dismissed" => ("✖️", "취소", "취소됨"),
        "rejected" | "denied" => ("✖️", "거절", "거절됨"),
        _ => ("☑️", "처리", "처리됨"),
    }
}

fn resolution_summary(decision: &str) -> &'static str {
    resolution_parts(decision).2
}

fn processed_title(text: &str, decision: &str) -> Option<String> {
    let title = ["⚠️", "⚠", ":warning:"]
        .into_iter()
        .find_map(|prefix| text.strip_prefix(prefix))?
        .trim_start()
        .strip_suffix("승인이 필요합니다")?
        .trim();
    if title.is_empty() {
        return None;
    }
    let (emoji, verb, _) = resolution_parts(decision);
    Some(format!("{emoji} {title} {verb}됨"))
}

pub(super) fn status_text(decision: &str, via: &str) -> String {
    let (emoji, verb, _) = resolution_parts(decision);
    let source = match via {
        "slack" => format!("Slack에서 {verb}"),
        "mac" => format!("맥에서 {verb}"),
        "ctl" => format!("ctl에서 {verb}"),
        "auto" if decision == "approved" => "자동 승인".to_string(),
        "auto" => "자동 처리".to_string(),
        _ => format!("{via}에서 {verb}"),
    };
    format!(
        "{emoji} {source} · {}",
        chrono::Local::now().format("%H:%M")
    )
}

fn processed_message_blocks(original: Option<&[Value]>, decision: &str, via: &str) -> Value {
    let Some(original) = original else {
        return json!([{ "type": "context", "elements": [
            { "type": "mrkdwn", "text": status_text(decision, via) }
        ] }]);
    };
    let mut blocks = original.to_vec();
    let mut title_section_seen = false;
    let mut status_position = None;
    let mut preserved = Vec::with_capacity(blocks.len() + 1);
    for block in blocks.drain(..) {
        if block["type"] == "actions" {
            status_position.get_or_insert(preserved.len());
            continue;
        }
        let mut block = block;
        if !title_section_seen && block["type"] == "section" {
            title_section_seen = true;
            if let Some(title) = block["text"]["text"].as_str() {
                if let Some(title) = processed_title(title, decision) {
                    block["text"]["text"] = Value::String(title);
                }
            }
        }
        preserved.push(block);
    }
    if preserved.len() < SLACK_BLOCK_LIMIT {
        let status = json!({
            "type": "context",
            "elements": [{ "type": "mrkdwn", "text": status_text(decision, via) }]
        });
        let position = status_position
            .unwrap_or(preserved.len())
            .min(preserved.len());
        preserved.insert(position, status);
    }
    Value::Array(preserved)
}

/// 소켓 수신 생존 감시. 시간은 호출자가 넣는다 (테스트에서 sleep 없이 검증).
#[derive(Debug)]
struct Liveness {
    last_rx: Instant,
    last_ping: Instant,
}

#[derive(Debug, PartialEq, Eq)]
enum Probe {
    Idle,
    Ping,
    Dead,
}

impl Liveness {
    fn new(now: Instant) -> Self {
        Self {
            last_rx: now,
            last_ping: now,
        }
    }

    /// pong·ping·envelope 등 어떤 프레임이든 받았을 때 호출한다.
    fn on_frame(&mut self, now: Instant) {
        self.last_rx = now;
    }

    fn probe(&mut self, now: Instant) -> Probe {
        if now.saturating_duration_since(self.last_rx) >= LIVENESS_TIMEOUT {
            Probe::Dead
        } else if now.saturating_duration_since(self.last_ping) >= PING_INTERVAL {
            self.last_ping = now;
            Probe::Ping
        } else {
            Probe::Idle
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ConnState {
    Disabled,
    Connected,
    Reconnecting,
    NoToken,
}

impl ConnState {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Disabled => "off",
            Self::Connected => "connected",
            Self::Reconnecting => "reconnecting",
            Self::NoToken => "no_token",
        }
    }

    fn menu_text(self) -> &'static str {
        match self {
            Self::Disabled => "Slack: 꺼짐",
            Self::Connected => "Slack: 연결됨",
            Self::Reconnecting => "Slack: ⚠️ 재연결 중",
            Self::NoToken => "Slack: ⚠️ 토큰 없음",
        }
    }
}

/// 연결 상태와 끊김 지속 시간. 끊김 알림은 같은 끊김 동안 한 번만 허용한다.
#[derive(Debug)]
struct Health {
    state: ConnState,
    down_since: Option<Instant>,
    alerted: bool,
}

impl Health {
    const fn new() -> Self {
        Self {
            state: ConnState::Disabled,
            down_since: None,
            alerted: false,
        }
    }

    fn set(&mut self, state: ConnState, now: Instant) {
        match state {
            ConnState::Disabled | ConnState::Connected => {
                self.down_since = None;
                self.alerted = false;
            }
            ConnState::Reconnecting | ConnState::NoToken => {
                self.down_since.get_or_insert(now);
            }
        }
        self.state = state;
    }

    /// 끊김이 임계를 넘겼고 아직 알리지 않았으면 true (한 번만).
    fn take_alert(&mut self, now: Instant) -> bool {
        match self.down_since {
            Some(since)
                if !self.alerted
                    && now.saturating_duration_since(since) >= DISCONNECT_ALERT_AFTER =>
            {
                self.alerted = true;
                true
            }
            _ => false,
        }
    }

    fn down_secs(&self, now: Instant) -> Option<u64> {
        self.down_since
            .map(|since| now.saturating_duration_since(since).as_secs())
    }
}

static HEALTH: Mutex<Health> = Mutex::new(Health::new());

fn set_conn_state(state: ConnState) {
    if let Ok(mut health) = HEALTH.lock() {
        health.set(state, Instant::now());
    }
}

/// 꺼짐 상태에서 연결을 시도하기 시작하면 재연결 중으로 올린다 (끊김 시계 시작).
fn mark_connecting() {
    if let Ok(mut health) = HEALTH.lock() {
        if health.state == ConnState::Disabled {
            health.set(ConnState::Reconnecting, Instant::now());
        }
    }
}

fn take_disconnect_alert() -> bool {
    HEALTH
        .lock()
        .map(|mut health| health.take_alert(Instant::now()))
        .unwrap_or(false)
}

pub(super) fn menu_text() -> &'static str {
    HEALTH
        .lock()
        .map(|health| health.state.menu_text())
        .unwrap_or(ConnState::Disabled.menu_text())
}

/// `knock ctl status` / `ctl list` 용 상태 JSON.
pub(super) fn status_json() -> Value {
    let Ok(health) = HEALTH.lock() else {
        return json!({ "state": "unknown" });
    };
    json!({
        "state": health.state.label(),
        "downSeconds": health.down_secs(Instant::now()),
    })
}

fn token_retry_delay(failures: u32) -> Duration {
    let exponent = failures.saturating_sub(1).min(6);
    Duration::from_secs((TOKEN_RETRY_BASE_SECS << exponent).min(TOKEN_RETRY_MAX_SECS))
}

pub(super) fn reconnect_backoff(attempt: u32) -> Duration {
    Duration::from_secs(2_u64.pow(attempt.min(6)).min(MAX_BACKOFF_SECS))
}

pub(super) fn envelope_ack(envelope_id: &str) -> String {
    json!({ "envelope_id": envelope_id }).to_string()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SlackMessageRef {
    channel: String,
    ts: String,
}

#[derive(Clone)]
pub(super) struct Integration {
    messages: Arc<Mutex<HashMap<String, MessageState>>>,
    sender: mpsc::Sender<Work>,
}

#[derive(Clone)]
enum MessageState {
    Pending {
        blocks: Vec<Value>,
    },
    Posted {
        messages: Vec<SlackMessageRef>,
        blocks: Vec<Value>,
    },
    Processed {
        text: String,
        messages: Vec<SlackMessageRef>,
        blocks: Value,
    },
}

pub(super) enum Work {
    Post {
        id: String,
        title: String,
        body: String,
        action_url: Option<String>,
        /// 있으면 승인/취소 대신 질문별 선택 UI 를 단다 (`ask`).
        ask: Option<AskSpec>,
    },
    /// 기타 입력 모달 열기 (`views.open`). trigger_id 는 3초 안에 써야 하므로 실패해도 재시도하지 않는다.
    OpenModal {
        trigger_id: String,
        view: Value,
    },
    Update {
        id: String,
        text: String,
        message: SlackMessageRef,
        blocks: Option<Value>,
    },
    Action {
        payload: Value,
        config: SlackConfig,
    },
    SocketEnded {
        generation: u64,
    },
}

impl Integration {
    pub(super) fn new() -> (Self, mpsc::Receiver<Work>) {
        let (sender, receiver) = mpsc::channel();
        (
            Self {
                messages: Arc::new(Mutex::new(HashMap::new())),
                sender,
            },
            receiver,
        )
    }

    pub(super) fn queue_request(
        &self,
        id: String,
        title: String,
        body: String,
        action_url: Option<String>,
    ) {
        self.queue_post(id, title, body, action_url, None);
    }

    /// `ask` 질문을 선택 UI(버튼·체크박스·기타)로 보낸다.
    pub(super) fn queue_ask_request(&self, id: String, title: String, spec: AskSpec) {
        self.queue_post(id, title, String::new(), None, Some(spec));
    }

    fn queue_post(
        &self,
        id: String,
        title: String,
        body: String,
        action_url: Option<String>,
        ask: Option<AskSpec>,
    ) {
        let (_, blocks) = build_post_view(&id, &title, &body, action_url.as_deref(), ask.as_ref());
        self.messages
            .lock()
            .unwrap()
            .insert(id.clone(), MessageState::Pending { blocks });
        let _ = self.sender.send(Work::Post {
            id,
            title,
            body,
            action_url,
            ask,
        });
    }

    fn deliver_action(&self, payload: Value, config: SlackConfig) {
        let _ = self.sender.send(Work::Action { payload, config });
    }

    fn socket_ended(&self, generation: u64) {
        let _ = self.sender.send(Work::SocketEnded { generation });
    }

    fn enqueue(&self, work: Work) {
        let _ = self.sender.send(work);
    }

    pub(super) fn mark_processed(&self, id: &str, decision: &str, via: &str) {
        let (text, blocks, update) = {
            let mut messages = self.messages.lock().unwrap();
            let Some(state) = messages.get_mut(id) else {
                return;
            };
            let (messages, original) = match state {
                MessageState::Pending { blocks } => (Vec::new(), blocks.clone()),
                MessageState::Posted { messages, blocks } => (messages.clone(), blocks.clone()),
                MessageState::Processed { .. } => return,
            };
            let text = resolution_summary(decision).to_string();
            let blocks = processed_message_blocks(Some(&original), decision, via);
            *state = MessageState::Processed {
                text: text.clone(),
                messages: messages.clone(),
                blocks: blocks.clone(),
            };
            (text, blocks, messages)
        };
        for message in update {
            let _ = self.sender.send(Work::Update {
                id: id.to_string(),
                text: text.clone(),
                message,
                blocks: Some(blocks.clone()),
            });
        }
    }

    fn should_post(&self, id: &str) -> bool {
        matches!(
            self.messages.lock().unwrap().get(id),
            Some(MessageState::Pending { .. })
        )
    }

    fn should_post_or_forget(&self, id: &str) -> bool {
        let pending = self.should_post(id);
        if !pending {
            self.forget(id);
        }
        pending
    }

    fn resolve_action(&self, id: &str, payload: &Value, decision: &str) {
        let message = message_ref(payload);
        let payload_blocks = payload
            .get("message")
            .and_then(|message| message.get("blocks"))
            .and_then(Value::as_array)
            .cloned();
        let tracked = self.messages.lock().unwrap().contains_key(id);
        if tracked {
            if let Some(message) = message {
                if let Some(update) = self.posted(id, message) {
                    self.enqueue(update);
                }
            }
            self.mark_processed(id, decision, "slack");
        } else if let Some(message) = message {
            self.enqueue(Work::Update {
                id: String::new(),
                text: resolution_summary(decision).to_string(),
                message,
                blocks: Some(processed_message_blocks(
                    payload_blocks.as_deref(),
                    decision,
                    "slack",
                )),
            });
        }
    }

    fn posted(&self, id: &str, message: SlackMessageRef) -> Option<Work> {
        let mut messages = self.messages.lock().unwrap();
        let state = messages.get_mut(id)?;
        match state {
            MessageState::Pending { blocks } => {
                *state = MessageState::Posted {
                    messages: vec![message],
                    blocks: blocks.clone(),
                };
                None
            }
            MessageState::Posted { messages, .. } => {
                if !messages.contains(&message) {
                    messages.push(message);
                }
                None
            }
            MessageState::Processed {
                text,
                messages: posted,
                blocks,
            } => {
                if posted.contains(&message) {
                    return None;
                }
                posted.push(message.clone());
                Some(Work::Update {
                    id: id.to_string(),
                    text: text.clone(),
                    message,
                    blocks: Some(blocks.clone()),
                })
            }
        }
    }

    fn forget(&self, id: &str) {
        self.messages.lock().unwrap().remove(id);
    }
}

#[cfg(target_os = "macos")]
fn store_token(account: &str, token: &str) -> Result<(), String> {
    token_cache().update(|| {
        let mode = crate::token_store::Mode::from_config(&crate::read_config());
        let security = crate::token_store::SecurityCli { keychain: None };
        crate::token_store::store(
            mode,
            &security,
            &crate::token_store::FrameworkLegacy,
            &crate::token_store::token_file_path(),
            account,
            token,
        )?;
        bump_token_generation()
    })
}

#[cfg(not(target_os = "macos"))]
fn store_token(_account: &str, _token: &str) -> Result<(), String> {
    token_cache().update(|| Err("Slack 원격 승인은 macOS에서만 사용할 수 있습니다".into()))
}

#[cfg(target_os = "macos")]
fn read_token(account: &str) -> Result<String, String> {
    let mode = crate::token_store::Mode::from_config(&crate::read_config());
    let security = crate::token_store::SecurityCli { keychain: None };
    crate::token_store::load(
        mode,
        &security,
        &crate::token_store::FrameworkLegacy,
        &crate::token_store::token_file_path(),
        account,
    )
}

#[cfg(not(target_os = "macos"))]
fn read_token(_account: &str) -> Result<String, String> {
    Err("Slack 원격 승인은 macOS에서만 사용할 수 있습니다".into())
}

fn validate_tokens(app_token: &str, bot_token: &str) -> Result<(), String> {
    if !app_token.starts_with("xapp-") || !bot_token.starts_with("xoxb-") {
        return Err("App Token은 xapp-, Bot Token은 xoxb- 형식이어야 합니다".into());
    }
    Ok(())
}

fn save_tokens(app_token: Option<String>, bot_token: Option<String>) -> Result<(), String> {
    let app_token = app_token
        .map(|token| token.trim().to_string())
        .filter(|token| !token.is_empty());
    let bot_token = bot_token
        .map(|token| token.trim().to_string())
        .filter(|token| !token.is_empty());
    if app_token.is_some() || bot_token.is_some() {
        if let (Some(app), Some(bot)) = (&app_token, &bot_token) {
            validate_tokens(app, bot)?;
        }
    }
    if let Some(token) = app_token {
        if !token.starts_with("xapp-") {
            return Err("App Token은 xapp- 형식이어야 합니다".into());
        }
        store_token("app_token", &token)?;
    }
    if let Some(token) = bot_token {
        if !token.starts_with("xoxb-") {
            return Err("Bot Token은 xoxb- 형식이어야 합니다".into());
        }
        store_token("bot_token", &token)?;
    }
    Ok(())
}

#[derive(Default)]
struct TokenCache {
    tokens: Mutex<Option<CacheEntry>>,
}

type TokenResult = Result<(String, String), String>;

struct CacheEntry {
    generation: u64,
    result: TokenResult,
    /// 연속 실패 횟수. 성공하면 0.
    failures: u32,
    /// 실패 결과를 이 시각까지 재사용한다 (그 전엔 키체인을 다시 읽지 않는다).
    retry_at: Option<Instant>,
}

impl TokenCache {
    fn read(
        &self,
        generation: u64,
        load: impl FnOnce() -> Result<(String, String), String>,
    ) -> Result<(String, String), String> {
        self.read_at(generation, Instant::now(), load)
    }

    /// 성공은 세대가 바뀔 때까지 캐시한다. 실패는 backoff 동안만 캐시하고 이후엔 다시 읽는다.
    fn read_at(
        &self,
        generation: u64,
        now: Instant,
        load: impl FnOnce() -> Result<(String, String), String>,
    ) -> Result<(String, String), String> {
        let mut cached = self
            .tokens
            .lock()
            .map_err(|_| "Slack 토큰 캐시를 사용할 수 없습니다".to_string())?;
        let mut failures = 0;
        if let Some(entry) = cached.as_ref() {
            if entry.generation == generation {
                if entry.result.is_ok() || entry.retry_at.is_some_and(|at| now < at) {
                    return entry.result.clone();
                }
                failures = entry.failures;
            }
        }
        let result = load();
        let (failures, retry_at) = match &result {
            Ok(_) => (0, None),
            Err(_) => {
                let failures = failures.saturating_add(1);
                (failures, Some(now + token_retry_delay(failures)))
            }
        };
        *cached = Some(CacheEntry {
            generation,
            result: result.clone(),
            failures,
            retry_at,
        });
        result
    }

    fn update(&self, save: impl FnOnce() -> Result<(), String>) -> Result<(), String> {
        let mut cached = self
            .tokens
            .lock()
            .map_err(|_| "Slack 토큰 캐시를 사용할 수 없습니다".to_string())?;
        let result = save();
        *cached = None;
        result
    }
}

static TOKEN_CACHE: OnceLock<TokenCache> = OnceLock::new();

fn token_cache() -> &'static TokenCache {
    TOKEN_CACHE.get_or_init(TokenCache::default)
}

#[cfg(target_os = "macos")]
// 설정 창은 별도 프로세스이므로 데몬에도 토큰 변경을 알린다.
fn bump_token_generation() -> Result<(), String> {
    crate::update_config_at(&crate::config_path(), |config| {
        let generation = config
            .get("slack_token_generation")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        config.insert(
            "slack_token_generation".into(),
            json!(generation.saturating_add(1)),
        );
        Ok(())
    })
    .map_err(|_| "Slack 토큰 변경을 설정에 기록하지 못했습니다".into())
}

fn load_tokens() -> Result<(String, String), String> {
    let app = read_token("app_token")?;
    let bot = read_token("bot_token")?;
    validate_tokens(&app, &bot)?;
    Ok((app, bot))
}

fn read_tokens() -> Result<(String, String), String> {
    let generation = crate::read_config()["slack_token_generation"]
        .as_u64()
        .unwrap_or(0);
    token_cache().read(generation, load_tokens)
}

#[tauri::command]
pub(super) async fn save_slack(
    enabled: bool,
    owner_user_id: String,
    remote_action_hosts: Vec<String>,
    remote_default: bool,
    app_token: Option<String>,
    bot_token: Option<String>,
) -> Result<(), String> {
    let owner_user_id = owner_user_id.trim().to_string();
    let remote_action_hosts = normalize_hosts(remote_action_hosts)?;
    tauri::async_runtime::spawn_blocking(move || {
        #[cfg(not(target_os = "macos"))]
        return Err("Slack 원격 승인은 macOS에서만 사용할 수 있습니다".into());
        #[cfg(target_os = "macos")]
        save_tokens(app_token, bot_token)?;
        #[cfg(not(target_os = "macos"))]
        let _ = (app_token, bot_token);
        crate::update_config_at(&crate::config_path(), |config| {
            config.insert(
                "slack".into(),
                json!({
                    "enabled": enabled,
                    "owner_user_id": owner_user_id,
                    "remote_action_hosts": remote_action_hosts,
                    "remote_default": remote_default,
                }),
            );
            Ok(())
        })
    })
    .await
    .map_err(|_| "Slack 설정 저장 작업을 완료하지 못했습니다".to_string())?
}

#[tauri::command]
pub(super) async fn test_slack() -> Result<String, String> {
    #[cfg(not(target_os = "macos"))]
    {
        return Err("Slack 원격 승인은 macOS에서만 사용할 수 있습니다".into());
    }
    #[cfg(target_os = "macos")]
    {
        tauri::async_runtime::spawn_blocking(|| {
            let expected = stamp();
            let config = &expected.0;
            ensure_test_enabled(config)?;
            if config.owner_user_id.is_empty() {
                return Err("Slack owner 사용자 ID를 먼저 입력하세요".into());
            }
            let (app, bot) = read_tokens()?;
            let client = api_client()?;
            api_call(&client, &bot, "auth.test", json!({}), Some(&expected))?;
            open_dm(&client, &bot, &config.owner_user_id, Some(&expected))?;
            let socket_url = api_call(
                &client,
                &app,
                "apps.connections.open",
                json!({}),
                Some(&expected),
            )?
            .get("url")
            .and_then(Value::as_str)
            .ok_or("Slack Socket URL을 받지 못했습니다")?
            .to_string();
            if stamp() != expected {
                return Err("Slack 설정이 변경되었습니다".into());
            }
            drop(connect_socket(&socket_url)?);
            Ok("Slack 연결 시험 성공 (Bot Token, DM, Socket Mode 확인)".to_string())
        })
        .await
        .map_err(|_| "Slack 연결 시험 작업을 완료하지 못했습니다".to_string())?
    }
}

fn ensure_test_enabled(config: &SlackConfig) -> Result<(), String> {
    if !config.enabled {
        Err("Slack을 활성화한 뒤 연결 시험을 실행하세요".into())
    } else {
        Ok(())
    }
}

fn api_client() -> Result<Client, String> {
    Client::builder()
        .timeout(API_TIMEOUT)
        .build()
        .map_err(|_| "Slack API 클라이언트를 시작할 수 없습니다".into())
}

fn api_call(
    client: &Client,
    token: &str,
    method: &str,
    body: Value,
    expected: Option<&(SlackConfig, Option<SystemTime>)>,
) -> Result<Value, String> {
    if expected.is_some_and(|expected| &stamp() != expected) {
        return Err("Slack 설정이 변경되었습니다".into());
    }
    let response = client
        .post(format!("https://slack.com/api/{method}"))
        .bearer_auth(token)
        .json(&body)
        .send()
        .map_err(|_| "Slack API 요청에 실패했습니다".to_string())?;
    let result: Value = response
        .json()
        .map_err(|_| "Slack API 응답을 읽을 수 없습니다".to_string())?;
    if result.get("ok").and_then(Value::as_bool) == Some(true) {
        Ok(result)
    } else {
        Err("Slack API가 요청을 거절했습니다".into())
    }
}

fn open_dm(
    client: &Client,
    bot_token: &str,
    owner_user_id: &str,
    expected: Option<&(SlackConfig, Option<SystemTime>)>,
) -> Result<String, String> {
    api_call(
        client,
        bot_token,
        "conversations.open",
        json!({ "users": owner_user_id }),
        expected,
    )?
    .get("channel")
    .and_then(|channel| channel.get("id"))
    .and_then(Value::as_str)
    .map(str::to_string)
    .ok_or_else(|| "Slack DM 채널을 열 수 없습니다".into())
}

fn post_message(
    client: &Client,
    bot_token: &str,
    dm_channel: &str,
    work: &Work,
    expected: Option<&(SlackConfig, Option<SystemTime>)>,
) -> Result<SlackMessageRef, String> {
    let Work::Post {
        id,
        title,
        body,
        action_url,
        ask,
    } = work
    else {
        return Err("Slack post 작업이 올바르지 않습니다".into());
    };
    let (fallback, blocks) = build_post_view(id, title, body, action_url.as_deref(), ask.as_ref());
    let result = api_call(
        client,
        bot_token,
        "chat.postMessage",
        json!({
            "channel": dm_channel,
            "text": fallback,
            "blocks": blocks,
            "unfurl_links": false,
            "unfurl_media": false,
        }),
        expected,
    )?;
    Ok(SlackMessageRef {
        channel: result
            .get("channel")
            .and_then(Value::as_str)
            .ok_or("Slack 메시지 채널을 찾을 수 없습니다")?
            .into(),
        ts: result
            .get("ts")
            .and_then(Value::as_str)
            .ok_or("Slack 메시지 ID를 찾을 수 없습니다")?
            .into(),
    })
}

fn update_message(
    client: &Client,
    bot_token: &str,
    message: &SlackMessageRef,
    text: &str,
    blocks: Option<Value>,
    expected: Option<&(SlackConfig, Option<SystemTime>)>,
) -> Result<(), String> {
    api_call(
        client,
        bot_token,
        "chat.update",
        json!({
            "channel": message.channel,
            "ts": message.ts,
            "text": mask_secrets(text),
            "blocks": blocks.unwrap_or_else(|| json!([])),
            "unfurl_links": false,
            "unfurl_media": false,
        }),
        expected,
    )?;
    Ok(())
}

fn message_ref(payload: &Value) -> Option<SlackMessageRef> {
    let channel = payload
        .get("channel")
        .and_then(|value| value.get("id"))
        .or_else(|| {
            payload
                .get("container")
                .and_then(|value| value.get("channel_id"))
        })?
        .as_str()?
        .to_string();
    let ts = payload
        .get("message")
        .and_then(|value| value.get("ts"))
        .or_else(|| {
            payload
                .get("container")
                .and_then(|value| value.get("message_ts"))
        })?;
    let ts = ts
        .as_str()
        .map(str::to_string)
        .or_else(|| ts.as_number().map(ToString::to_string))?;
    Some(SlackMessageRef { channel, ts })
}

pub(super) enum ActionResult {
    Applied {
        id: String,
        decision: &'static str,
    },
    AppliedMany {
        clicked_id: String,
        ids: Vec<String>,
        remote_ids: Vec<String>,
    },
    Rejected {
        message: Option<SlackMessageRef>,
        blocks: Option<Value>,
    },
    /// 아직 다 답하지 않은 `ask`: 선택 표시를 반영한 블록으로 메시지를 갱신한다.
    AskUpdate {
        message: Option<SlackMessageRef>,
        text: String,
        blocks: Vec<Value>,
    },
    OpenModal {
        trigger_id: String,
        view: Value,
    },
    Ignored,
}

pub(super) fn process_block_action(
    enabled: bool,
    owner_user_id: &str,
    queue: &Arc<Mutex<Vec<crate::QueueEntry>>>,
    payload: &Value,
) -> ActionResult {
    let needs_global_touch_id = enabled
        && payload
            .get("actions")
            .and_then(Value::as_array)
            .and_then(|actions| actions.first())
            .and_then(|action| action.get("action_id"))
            .and_then(Value::as_str)
            == Some("knock_approve_all");
    process_block_action_with_touch_id(
        enabled,
        owner_user_id,
        queue,
        payload,
        needs_global_touch_id && crate::config_touch_id(),
    )
}

fn process_block_action_with_touch_id(
    enabled: bool,
    owner_user_id: &str,
    queue: &Arc<Mutex<Vec<crate::QueueEntry>>>,
    payload: &Value,
    config_touch_id: bool,
) -> ActionResult {
    if !enabled {
        eprintln!("knock: Slack action ignored (slack disabled)");
        return ActionResult::Ignored;
    }
    if payload
        .get("user")
        .and_then(|user| user.get("id"))
        .and_then(Value::as_str)
        != Some(owner_user_id)
    {
        eprintln!("knock: Slack action rejected (user is not the configured owner)");
        let blocks = payload
            .get("message")
            .and_then(|message| message.get("blocks"))
            .and_then(Value::as_array)
            .map(|blocks| {
                let mut blocks = blocks.clone();
                let warning = "⛔ 이 요청은 Slack owner만 결정할 수 있습니다";
                let warned = blocks.iter().any(|block| {
                    block["type"] == "context"
                        && block["elements"].as_array().is_some_and(|elements| {
                            elements.iter().any(|element| element["text"] == warning)
                        })
                });
                if !warned && blocks.len() < SLACK_BLOCK_LIMIT {
                    blocks.push(json!({ "type": "context", "elements": [
                    { "type": "mrkdwn", "text": warning }
                ] }));
                }
                Value::Array(blocks)
            });
        return ActionResult::Rejected {
            message: message_ref(payload),
            blocks,
        };
    }
    if payload.get("type").and_then(Value::as_str) == Some("view_submission") {
        return process_view_submission(queue, payload);
    }
    let Some(action) = payload
        .get("actions")
        .and_then(Value::as_array)
        .and_then(|actions| actions.first())
    else {
        eprintln!("knock: Slack action ignored (no actions in payload)");
        return ActionResult::Ignored;
    };
    if let Some(qi) = action
        .get("action_id")
        .and_then(Value::as_str)
        .and_then(|action_id| action_id.strip_prefix("knock_ask_other_"))
        .and_then(|qi| qi.parse::<usize>().ok())
    {
        return open_other_modal(queue, action, qi, payload);
    }
    if let Some((id, edit)) = parse_ask_edit(action) {
        return apply_ask_edit(queue, &id, edit, message_ref(payload));
    }
    if action.get("action_id").and_then(Value::as_str) == Some("knock_approve_all") {
        let Some(clicked_id) = action.get("value").and_then(Value::as_str) else {
            eprintln!("knock: Slack bulk action ignored (no value on button)");
            return ActionResult::Ignored;
        };
        // The clicked card anchors the bulk operation. Ignore stale cards and
        // cards which no longer belong to Slack, so an old button cannot act
        // on requests that arrived later.
        if !queue
            .lock()
            .unwrap()
            .iter()
            .any(|entry| entry.id == clicked_id && entry.remote_slack)
        {
            eprintln!(
                "knock: Slack bulk action ignored (no pending remote entry for id={clicked_id})"
            );
            return ActionResult::Ignored;
        }
        let candidates: Vec<String> = queue
            .lock()
            .unwrap()
            .iter()
            // Slack 으로 보낸 건만 대상이다. --local-only·Slack 비대상 게이트는 맥 앞에서만
            // 승인해야 하므로 Slack 버튼이 건드리지 않는다 (ctl approve all 은 로컬이라 예외).
            .filter(|entry| {
                entry.remote_slack
                    && crate::bulk_approve_skip_reason(
                        &entry.kind,
                        &entry.payload,
                        entry.in_progress,
                        config_touch_id,
                    )
                    .is_none()
            })
            .map(|entry| entry.id.clone())
            .collect();
        let response = json!({ "decision": "approved", "via": "slack" });
        let mut ids = Vec::new();
        let mut remote_ids = Vec::new();
        for id in candidates {
            let resolved = crate::reply_queue_entry(
                queue,
                &id,
                &response,
                |entry| {
                    entry.remote_slack
                        && crate::bulk_approve_skip_reason(
                            &entry.kind,
                            &entry.payload,
                            entry.in_progress,
                            config_touch_id,
                        )
                        .is_none()
                },
                |_| {},
            );
            if let Some(resolved) = resolved {
                if resolved.remote_slack {
                    remote_ids.push(id.clone());
                }
                ids.push(id);
            }
        }
        return ActionResult::AppliedMany {
            clicked_id: clicked_id.to_string(),
            ids,
            remote_ids,
        };
    }
    let decision = match action.get("action_id").and_then(Value::as_str) {
        Some("knock_approve") => "approved",
        Some("knock_dismiss") => "dismissed",
        other => {
            eprintln!("knock: Slack action ignored (unknown action_id={other:?})");
            return ActionResult::Ignored;
        }
    };
    let Some(id) = action.get("value").and_then(Value::as_str) else {
        eprintln!("knock: Slack action ignored (no value on button)");
        return ActionResult::Ignored;
    };
    let response = json!({ "decision": decision, "via": "slack" });
    if crate::reply_queue_entry(queue, id, &response, |entry| entry.remote_slack, |_| {}).is_none()
    {
        eprintln!("knock: Slack action ignored (no pending remote entry for id={id})");
        return ActionResult::Ignored;
    }
    ActionResult::Applied {
        id: id.to_string(),
        decision,
    }
}

/// 큐 항목에 적용할 `ask` 편집.
enum AskEdit {
    Pick(usize, usize),
    Check(usize, Vec<usize>),
    Other(usize, String),
    Submit,
}

/// 버튼/체크박스 action 을 `ask` 편집으로 푼다. 모르는 모양이면 None.
fn parse_ask_edit(action: &Value) -> Option<(String, AskEdit)> {
    let action_id = action.get("action_id").and_then(Value::as_str)?;
    let value = action
        .get("value")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let num = |text: &str| text.parse::<usize>().ok();
    if action_id == "knock_ask_submit" {
        return Some((value.to_string(), AskEdit::Submit));
    }
    if let Some(index) = action_id.strip_prefix("knock_ask_opt_").and_then(num) {
        // 질문 1개·단일 선택 버튼: value = `<id>|<옵션>`
        let (id, oi) = value.rsplit_once('|')?;
        return (num(oi)? == index).then(|| (id.to_string(), AskEdit::Pick(0, index)));
    }
    if let Some(rest) = action_id.strip_prefix("knock_ask_q") {
        let (qi, oi) = rest.split_once("_o")?;
        let (qi, oi) = (num(qi)?, num(oi)?);
        let (id, tail) = value.split_once('|')?;
        return (tail == format!("{qi}|{oi}")).then(|| (id.to_string(), AskEdit::Pick(qi, oi)));
    }
    if let Some(qi) = action_id.strip_prefix("knock_ask_chk_").and_then(num) {
        // 체크가 모두 풀리면 selected_options 가 비므로 id 는 block_id(`<질문>|<id>`)로 받는다.
        let (block_qi, id) = action
            .get("block_id")
            .and_then(Value::as_str)?
            .split_once('|')?;
        (num(block_qi)? == qi).then_some(())?;
        let picked = action
            .get("selected_options")?
            .as_array()?
            .iter()
            .filter_map(|option| option.get("value").and_then(Value::as_str))
            .filter_map(|value| {
                let mut parts = value.rsplitn(3, '|');
                let oi = num(parts.next()?)?;
                (num(parts.next()?)? == qi).then_some(oi)
            })
            .collect();
        return Some((id.to_string(), AskEdit::Check(qi, picked)));
    }
    None
}

/// `ask` 편집을 큐 항목에 적용한다. 모두 답해졌으면(복수 선택이 있으면 제출까지) 맥 창과 같은 형식의
/// answers 로 답하고, 아니면 갱신된 블록을 돌려준다.
fn apply_ask_edit(
    queue: &Arc<Mutex<Vec<crate::QueueEntry>>>,
    id: &str,
    edit: AskEdit,
    message: Option<SlackMessageRef>,
) -> ActionResult {
    enum Outcome {
        Done(Value),
        Progress(String, Vec<Value>),
        Ignore,
    }
    let outcome = {
        let mut queue = queue.lock().unwrap();
        let Some(entry) = queue
            .iter_mut()
            .find(|entry| entry.id == id && entry.remote_slack && entry.kind == "ask")
        else {
            eprintln!("knock: Slack action ignored (no pending remote ask for id={id})");
            return ActionResult::Ignored;
        };
        let Some(spec) = ask_remote_spec(&entry.payload) else {
            eprintln!("knock: Slack action ignored (ask is not remote-capable, id={id})");
            return ActionResult::Ignored;
        };
        entry
            .ask_picks
            .resize(spec.questions.len(), AskPick::default());
        let submit = matches!(edit, AskEdit::Submit);
        let picks = &mut entry.ask_picks;
        let ok = match edit {
            AskEdit::Pick(qi, oi) => spec
                .questions
                .get(qi)
                .filter(|question| !question.multi && oi < question.labels.len())
                .map(|_| {
                    picks[qi] = AskPick {
                        sel: vec![oi],
                        other: None,
                    }
                })
                .is_some(),
            AskEdit::Check(qi, mut picked) => spec
                .questions
                .get(qi)
                .filter(|question| question.multi)
                .map(|question| {
                    picked.retain(|&oi| oi < question.labels.len());
                    picked.sort_unstable();
                    picked.dedup();
                    picks[qi].sel = picked;
                })
                .is_some(),
            AskEdit::Other(qi, text) => spec
                .questions
                .get(qi)
                .map(|question| {
                    if !question.multi {
                        picks[qi].sel.clear();
                    }
                    picks[qi].other = Some(text);
                })
                .is_some(),
            AskEdit::Submit => true,
        };
        if !ok {
            eprintln!("knock: Slack action ignored (ask edit out of range, id={id})");
            Outcome::Ignore
        } else {
            let all = spec
                .questions
                .iter()
                .zip(picks.iter())
                .all(|(question, pick)| !ask_labels(question, pick).is_empty());
            if all && (!spec.any_multi() || submit) {
                let answers: serde_json::Map<String, Value> = spec
                    .questions
                    .iter()
                    .zip(picks.iter())
                    .map(|(question, pick)| {
                        (question.key.clone(), json!(ask_labels(question, pick)))
                    })
                    .collect();
                Outcome::Done(json!({ "answers": answers, "decision": "approved", "via": "slack" }))
            } else {
                let notice =
                    (submit && !all).then_some("⚠️ 선택하지 않은 질문이 있어 제출하지 않았습니다");
                let title = entry
                    .payload
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or("Knock");
                let (text, blocks) = build_ask_blocks(id, title, &spec, picks, notice);
                Outcome::Progress(text, blocks)
            }
        }
    };
    match outcome {
        Outcome::Ignore => ActionResult::Ignored,
        Outcome::Progress(text, blocks) => ActionResult::AskUpdate {
            message,
            text,
            blocks,
        },
        Outcome::Done(response) => {
            if crate::reply_queue_entry(queue, id, &response, |entry| entry.remote_slack, |_| {})
                .is_none()
            {
                eprintln!("knock: Slack action ignored (no pending remote entry for id={id})");
                return ActionResult::Ignored;
            }
            ActionResult::Applied {
                id: id.to_string(),
                decision: "approved",
            }
        }
    }
}

/// `기타…` 버튼: 입력 모달을 연다. 현재 입력(있으면)을 미리 채운다.
fn open_other_modal(
    queue: &Arc<Mutex<Vec<crate::QueueEntry>>>,
    action: &Value,
    qi: usize,
    payload: &Value,
) -> ActionResult {
    let id = action
        .get("value")
        .and_then(Value::as_str)
        .and_then(|value| value.split_once('|'))
        .filter(|(_, tail)| tail.parse::<usize>().ok() == Some(qi))
        .map(|(id, _)| id.to_string());
    let (Some(id), Some(trigger_id), Some(message)) = (
        id,
        payload.get("trigger_id").and_then(Value::as_str),
        message_ref(payload),
    ) else {
        eprintln!("knock: Slack action ignored (malformed ask other button)");
        return ActionResult::Ignored;
    };
    let found = {
        let queue = queue.lock().unwrap();
        queue
            .iter()
            .find(|entry| entry.id == id && entry.remote_slack && entry.kind == "ask")
            .and_then(|entry| {
                let spec = ask_remote_spec(&entry.payload)?;
                let question = spec.questions.get(qi)?.clone();
                let current = entry.ask_picks.get(qi).and_then(|pick| pick.other.clone());
                Some((question, current))
            })
    };
    let Some((question, current)) = found else {
        eprintln!("knock: Slack action ignored (no pending remote ask for id={id})");
        return ActionResult::Ignored;
    };
    let mut element = json!({ "type": "plain_text_input", "action_id": "text", "multiline": true });
    if let Some(current) = current.filter(|text| !text.is_empty()) {
        element["initial_value"] = json!(current);
    }
    let view = json!({
        "type": "modal",
        "callback_id": "knock_ask_other",
        "private_metadata": json!({ "id": id, "qi": qi,
            "channel": message.channel, "ts": message.ts }).to_string(),
        "title": { "type": "plain_text", "text": "기타 입력" },
        "submit": { "type": "plain_text", "text": "확인" },
        "close": { "type": "plain_text", "text": "닫기" },
        "blocks": [{ "type": "input", "block_id": "other",
            "label": { "type": "plain_text", "text": ask_plain(&question.key, 200) },
            "element": element }]
    });
    ActionResult::OpenModal {
        trigger_id: trigger_id.to_string(),
        view,
    }
}

/// 기타 모달 제출(`view_submission`): 그 질문의 답으로 넣는다. 소유자 검사는 호출 전에 끝나 있다.
fn process_view_submission(
    queue: &Arc<Mutex<Vec<crate::QueueEntry>>>,
    payload: &Value,
) -> ActionResult {
    let view = &payload["view"];
    if view["callback_id"] != "knock_ask_other" {
        return ActionResult::Ignored;
    }
    let meta: Value = view["private_metadata"]
        .as_str()
        .and_then(|raw| serde_json::from_str(raw).ok())
        .unwrap_or(Value::Null);
    let (Some(id), Some(qi)) = (
        meta["id"].as_str(),
        meta["qi"].as_u64().map(|qi| qi as usize),
    ) else {
        eprintln!("knock: Slack view ignored (malformed metadata)");
        return ActionResult::Ignored;
    };
    let text = view["state"]["values"]["other"]["text"]["value"]
        .as_str()
        .map(str::trim)
        .unwrap_or_default();
    if text.is_empty() {
        eprintln!("knock: Slack view ignored (empty other text)");
        return ActionResult::Ignored;
    }
    let message = match (meta["channel"].as_str(), meta["ts"].as_str()) {
        (Some(channel), Some(ts)) => Some(SlackMessageRef {
            channel: channel.into(),
            ts: ts.into(),
        }),
        _ => None,
    };
    apply_ask_edit(queue, id, AskEdit::Other(qi, text.to_string()), message)
}

fn stamp() -> (SlackConfig, Option<SystemTime>) {
    let path = crate::config_path();
    let config = config_from_value(&crate::read_config_at(&path));
    let modified = std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok();
    (config, modified)
}

type ConfigVersion = (SlackConfig, Option<SystemTime>);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StartError {
    Credentials,
    Connection,
}

fn connect_if_enabled<T, S>(
    config: &SlackConfig,
    load_credentials: impl FnOnce() -> Result<T, String>,
    connect: impl FnOnce(T, &SlackConfig) -> Result<S, String>,
) -> Result<Option<S>, StartError> {
    if !config.enabled || config.owner_user_id.is_empty() {
        return Ok(None);
    }
    let credentials = load_credentials().map_err(|_| StartError::Credentials)?;
    connect(credentials, config)
        .map(Some)
        .map_err(|_| StartError::Connection)
}

fn connect_socket(url: &str) -> Result<WebSocket<MaybeTlsStream<TcpStream>>, String> {
    let parsed = Url::parse(url).map_err(|_| "Slack Socket URL이 올바르지 않습니다")?;
    let host = parsed
        .host_str()
        .ok_or("Slack Socket URL에 호스트가 없습니다")?;
    let port = parsed
        .port_or_known_default()
        .ok_or("Slack Socket URL 포트가 없습니다")?;
    let addresses = (host, port)
        .to_socket_addrs()
        .map_err(|_| "Slack Socket 주소를 확인할 수 없습니다")?;
    let stream = addresses
        .filter_map(|address| TcpStream::connect_timeout(&address, SOCKET_CONNECT_TIMEOUT).ok())
        .next()
        .ok_or("Slack Socket 연결에 실패했습니다")?;
    stream
        .set_read_timeout(Some(SOCKET_CONNECT_TIMEOUT))
        .map_err(|_| "Slack Socket timeout 설정 실패")?;
    stream
        .set_write_timeout(Some(SOCKET_CONNECT_TIMEOUT))
        .map_err(|_| "Slack Socket timeout 설정 실패")?;
    let (mut socket, _) = tungstenite::client_tls_with_config(url, stream, None, None)
        .map_err(|_| "Slack Socket handshake에 실패했습니다")?;
    set_socket_timeouts(&mut socket, SOCKET_POLL).map_err(|_| "Slack Socket timeout 설정 실패")?;
    Ok(socket)
}

fn set_socket_timeouts(
    socket: &mut WebSocket<MaybeTlsStream<TcpStream>>,
    timeout: Duration,
) -> io::Result<()> {
    match socket.get_mut() {
        MaybeTlsStream::Plain(stream) => {
            stream.set_read_timeout(Some(timeout))?;
            stream.set_write_timeout(Some(timeout))
        }
        MaybeTlsStream::NativeTls(stream) => {
            stream.get_ref().set_read_timeout(Some(timeout))?;
            stream.get_ref().set_write_timeout(Some(timeout))
        }
        _ => Ok(()),
    }
}

fn is_idle_io(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

fn run_socket_reader(
    mut socket: WebSocket<MaybeTlsStream<TcpStream>>,
    integration: Integration,
    config: SlackConfig,
    generation: u64,
    stop: Arc<AtomicBool>,
) {
    let mut liveness = Liveness::new(Instant::now());
    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        match liveness.probe(Instant::now()) {
            Probe::Dead => {
                eprintln!(
                    "knock: Slack socket silent for {}s; reconnecting",
                    LIVENESS_TIMEOUT.as_secs()
                );
                break;
            }
            Probe::Ping => match socket.send(Message::Ping(Vec::new().into())) {
                Ok(()) => {}
                // 쓰기가 막혀도 프레임은 큐에 남는다. 진짜 끊김은 수신 무소식으로 잡는다.
                Err(tungstenite::Error::Io(error)) if is_idle_io(&error) => {}
                Err(_) => break,
            },
            Probe::Idle => {}
        }
        let frame = socket.read();
        if frame.is_ok() {
            liveness.on_frame(Instant::now());
        }
        match frame {
            Ok(Message::Text(text)) => {
                let Ok(envelope) = serde_json::from_str::<Value>(&text) else {
                    continue;
                };
                if let Some(envelope_id) = envelope.get("envelope_id").and_then(Value::as_str) {
                    if socket
                        .send(Message::Text(envelope_ack(envelope_id).into()))
                        .is_err()
                    {
                        break;
                    }
                }
                match envelope.get("type").and_then(Value::as_str) {
                    Some("hello") => eprintln!("knock: Slack hello received"),
                    Some("disconnect") => {
                        eprintln!(
                            "knock: Slack disconnect requested (reason={})",
                            envelope
                                .get("reason")
                                .and_then(Value::as_str)
                                .unwrap_or("?")
                        );
                        break;
                    }
                    _ => {}
                }
                if envelope.get("type").and_then(Value::as_str) != Some("interactive") {
                    continue;
                }
                let payload = match envelope.get("payload") {
                    Some(Value::Object(_)) => envelope["payload"].clone(),
                    Some(Value::String(raw)) => serde_json::from_str(raw).unwrap_or(Value::Null),
                    _ => Value::Null,
                };
                eprintln!(
                    "knock: Slack interactive payload received (type={})",
                    payload.get("type").and_then(Value::as_str).unwrap_or("?")
                );
                if matches!(
                    payload.get("type").and_then(Value::as_str),
                    Some("block_actions" | "view_submission")
                ) {
                    integration.deliver_action(payload, config.clone());
                }
            }
            Ok(Message::Close(_)) => {
                eprintln!("knock: Slack socket closed by peer");
                break;
            }
            Ok(_) => {}
            Err(tungstenite::Error::Io(error)) if is_idle_io(&error) => {}
            Err(_) => {
                eprintln!("knock: Slack socket read failed");
                break;
            }
        }
    }
    integration.socket_ended(generation);
}

struct Session {
    bot_token: String,
    dm_channel: String,
    generation: u64,
    stop: Arc<AtomicBool>,
    reader: thread::JoinHandle<()>,
}

fn close_session(session: Option<Session>) {
    if let Some(session) = session {
        session.stop.store(true, Ordering::Relaxed);
        let _ = session.reader.join();
    }
}

fn notify_local_resolution(app: &tauri::AppHandle, queue: &Arc<Mutex<Vec<crate::QueueEntry>>>) {
    let count = queue.lock().unwrap().len();
    let app = app.clone();
    let _ = app.clone().run_on_main_thread(move || {
        let _ = app.emit("queue-changed", ());
        crate::update_badge(&app, count);
        if count == 0 {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.hide();
            }
        }
    });
}

fn handle_action(
    work: Work,
    version: &ConfigVersion,
    integration: &Integration,
    app: &tauri::AppHandle,
    queue: &Arc<Mutex<Vec<crate::QueueEntry>>>,
) -> Option<Work> {
    let Work::Action { payload, config } = work else {
        return None;
    };
    if !config.enabled || !version.0.enabled || config.owner_user_id != version.0.owner_user_id {
        return None;
    }
    match process_block_action(config.enabled, &config.owner_user_id, queue, &payload) {
        ActionResult::Applied { id, decision } => {
            integration.resolve_action(&id, &payload, decision);
            notify_local_resolution(app, queue);
            None
        }
        ActionResult::AppliedMany {
            clicked_id,
            ids,
            remote_ids,
        } => {
            for id in &remote_ids {
                if id == &clicked_id {
                    integration.resolve_action(id, &payload, "approved");
                } else {
                    integration.mark_processed(id, "approved", "slack");
                }
            }
            if !ids.is_empty() {
                notify_local_resolution(app, queue);
            }
            None
        }
        ActionResult::Rejected {
            message: Some(message),
            blocks,
        } => Some(Work::Update {
            id: String::new(),
            text: "⛔ 이 요청은 Slack owner만 결정할 수 있습니다".into(),
            message,
            blocks,
        }),
        ActionResult::AskUpdate {
            message: Some(message),
            text,
            blocks,
        } => Some(Work::Update {
            id: String::new(),
            text,
            message,
            blocks: Some(Value::Array(blocks)),
        }),
        ActionResult::OpenModal { trigger_id, view } => Some(Work::OpenModal { trigger_id, view }),
        ActionResult::Rejected { message: None, .. }
        | ActionResult::AskUpdate { message: None, .. }
        | ActionResult::Ignored => None,
    }
}

fn execute_http(
    work: &Work,
    session: &Session,
    version: &ConfigVersion,
    integration: &Integration,
    client: &Client,
) -> Result<(), ()> {
    match work {
        Work::Post { id, .. } => {
            if !integration.should_post_or_forget(id) {
                return Ok(());
            }
            match post_message(
                client,
                &session.bot_token,
                &session.dm_channel,
                work,
                Some(version),
            ) {
                Ok(message) => {
                    if let Some(update) = integration.posted(id, message) {
                        integration.enqueue(update);
                    }
                    Ok(())
                }
                Err(_) => Err(()),
            }
        }
        Work::Update {
            id,
            text,
            message,
            blocks,
        } => {
            if update_message(
                client,
                &session.bot_token,
                message,
                text,
                blocks.clone(),
                Some(version),
            )
            .is_err()
            {
                return Err(());
            }
            if !id.is_empty() {
                integration.forget(id);
            }
            Ok(())
        }
        Work::OpenModal { trigger_id, view } => {
            let opened = api_call(
                client,
                &session.bot_token,
                "views.open",
                json!({ "trigger_id": trigger_id, "view": view }),
                Some(version),
            );
            if opened.is_err() {
                eprintln!("knock: Slack views.open failed");
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn dispatch_or_hold(
    work: Work,
    connected: bool,
    pending: &mut VecDeque<Work>,
    execute: impl FnOnce(&Work) -> Result<(), ()>,
) -> bool {
    if !connected {
        if matches!(
            &work,
            Work::Post { .. } | Work::Update { .. } | Work::OpenModal { .. }
        ) {
            pending.push_back(work);
        }
        return false;
    }
    if execute(&work).is_err() {
        pending.push_front(work);
        true
    } else {
        false
    }
}

pub(super) fn run_worker(
    integration: Integration,
    receiver: mpsc::Receiver<Work>,
    app: tauri::AppHandle,
    queue: Arc<Mutex<Vec<crate::QueueEntry>>>,
) {
    let Ok(client) = api_client() else { return };
    let mut pending = VecDeque::new();
    let mut session: Option<Session> = None;
    let mut version = stamp();
    let mut attempt = 0_u32;
    let mut generation = 0_u64;
    let mut retry_at = Instant::now();
    let mut missing_tokens_logged = false;
    loop {
        let current = stamp();
        if current != version {
            close_session(session.take());
            version = current;
            attempt = 0;
            retry_at = Instant::now();
            missing_tokens_logged = false;
        }
        if !version.0.enabled || version.0.owner_user_id.is_empty() {
            set_conn_state(ConnState::Disabled);
        }
        if take_disconnect_alert() {
            let _ = app
                .notification()
                .builder()
                .title("Knock — Slack 연결 끊김")
                .body("Slack 연결이 5분 넘게 끊겨 있습니다. 폰 승인 버튼이 도착하지 않을 수 있습니다.")
                .show();
        }
        if !version.0.enabled {
            close_session(session.take());
            if let Ok(work) = receiver.recv_timeout(SOCKET_POLL) {
                dispatch_or_hold(work, false, &mut pending, |_| unreachable!());
            }
            continue;
        }
        if session.is_none() && !version.0.owner_user_id.is_empty() && Instant::now() >= retry_at {
            if stamp() != version {
                continue;
            }
            mark_connecting();
            eprintln!("knock: Slack connecting (attempt {})", attempt + 1);
            let connected = connect_if_enabled(
                &version.0,
                || {
                    if stamp() != version {
                        return Err("Slack 설정이 변경되었습니다".into());
                    }
                    read_tokens()
                },
                |(app_token, bot_token), config| {
                    if stamp() != version {
                        return Err("Slack 설정이 변경되었습니다".into());
                    }
                    let url = api_call(
                        &client,
                        &app_token,
                        "apps.connections.open",
                        json!({}),
                        Some(&version),
                    )?
                    .get("url")
                    .and_then(Value::as_str)
                    .ok_or("Slack Socket URL을 받지 못했습니다")?
                    .to_string();
                    let channel =
                        open_dm(&client, &bot_token, &config.owner_user_id, Some(&version))?;
                    if stamp() != version {
                        return Err("Slack 설정이 변경되었습니다".into());
                    }
                    let socket = connect_socket(&url)?;
                    if stamp() != version {
                        return Err("Slack 설정이 변경되었습니다".into());
                    }
                    Ok((bot_token, channel, socket))
                },
            );
            match connected {
                Err(StartError::Credentials) => {
                    if !missing_tokens_logged {
                        eprintln!("knock: Slack enabled but Keychain tokens are unavailable");
                        missing_tokens_logged = true;
                    }
                    set_conn_state(ConnState::NoToken);
                    retry_at = Instant::now() + reconnect_backoff(attempt);
                    attempt = attempt.saturating_add(1);
                }
                Err(StartError::Connection) => {
                    eprintln!("knock: Slack Socket Mode disconnected; reconnecting");
                    set_conn_state(ConnState::Reconnecting);
                    retry_at = Instant::now() + reconnect_backoff(attempt);
                    attempt = attempt.saturating_add(1);
                }
                Ok(None) => {}
                Ok(Some((bot_token, dm_channel, socket))) => {
                    generation = generation.wrapping_add(1);
                    let stop = Arc::new(AtomicBool::new(false));
                    let reader = {
                        let integration = integration.clone();
                        let config = version.0.clone();
                        let stop = stop.clone();
                        thread::spawn(move || {
                            run_socket_reader(socket, integration, config, generation, stop)
                        })
                    };
                    session = Some(Session {
                        bot_token,
                        dm_channel,
                        generation,
                        stop,
                        reader,
                    });
                    attempt = 0;
                    missing_tokens_logged = false;
                    set_conn_state(ConnState::Connected);
                    eprintln!("knock: Slack connected (generation {generation})");
                }
            }
        }

        let work = if session.is_some() {
            match receiver.try_recv() {
                Ok(work @ (Work::Action { .. } | Work::SocketEnded { .. })) => Some(work),
                Ok(work @ (Work::Post { .. } | Work::Update { .. } | Work::OpenModal { .. }))
                    if !pending.is_empty() =>
                {
                    pending.push_back(work);
                    pending.pop_front()
                }
                Ok(work) => Some(work),
                Err(mpsc::TryRecvError::Empty) => pending
                    .pop_front()
                    .or_else(|| receiver.recv_timeout(SOCKET_POLL).ok()),
                Err(mpsc::TryRecvError::Disconnected) => pending.pop_front(),
            }
        } else {
            receiver.recv_timeout(SOCKET_POLL).ok()
        };
        let Some(work) = work else { continue };
        match work {
            action @ Work::Action { .. } => {
                if let Some(update) = handle_action(action, &version, &integration, &app, &queue) {
                    pending.push_back(update);
                }
            }
            Work::SocketEnded {
                generation: ended_generation,
            } => {
                let current_generation = session.as_ref().map(|s| s.generation);
                if current_generation == Some(ended_generation) {
                    close_session(session.take());
                    set_conn_state(ConnState::Reconnecting);
                    eprintln!("knock: Slack socket ended; reconnecting");
                    retry_at = Instant::now() + reconnect_backoff(attempt);
                    attempt = attempt.saturating_add(1);
                }
            }
            work @ (Work::Post { .. } | Work::Update { .. } | Work::OpenModal { .. }) => {
                let failed = dispatch_or_hold(work, session.is_some(), &mut pending, |work| {
                    execute_http(
                        work,
                        session.as_ref().unwrap(),
                        &version,
                        &integration,
                        &client,
                    )
                });
                if failed {
                    eprintln!("knock: Slack message request failed; retrying");
                    close_session(session.take());
                    set_conn_state(ConnState::Reconnecting);
                    retry_at = Instant::now() + reconnect_backoff(attempt);
                    attempt = attempt.saturating_add(1);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::Barrier;

    type TestQueue = Arc<Mutex<Vec<crate::QueueEntry>>>;
    type TestResponse = Arc<Mutex<Vec<u8>>>;

    #[derive(Clone)]
    struct SharedWriter(Arc<Mutex<Vec<u8>>>);
    impl Write for SharedWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn queued(remote_slack: bool) -> (TestResponse, TestQueue) {
        let (entry, bytes) = queue_entry(
            "q-1",
            "annotate",
            json!({ "mode": "annotate" }),
            remote_slack,
            false,
        );
        (bytes, Arc::new(Mutex::new(vec![entry])))
    }

    fn queue_entry(
        id: &str,
        kind: &str,
        payload: Value,
        remote_slack: bool,
        in_progress: bool,
    ) -> (crate::QueueEntry, TestResponse) {
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let entry = crate::QueueEntry {
            in_progress,
            id: id.into(),
            kind: kind.into(),
            payload,
            source: json!({}),
            created_at: 1,
            remote_slack,
            ask_picks: Vec::new(),
            decision_via: None,
            responder: crate::ipc::Responder::new(SharedWriter(bytes.clone())),
        };
        (entry, bytes)
    }

    fn gate_entry(
        id: &str,
        remote: bool,
        gate: bool,
        touch_id: bool,
        in_progress: bool,
    ) -> (crate::QueueEntry, TestResponse) {
        queue_entry(
            id,
            "annotate",
            json!({ "gate": gate, "touchId": touch_id }),
            remote,
            in_progress,
        )
    }

    fn click(queue: &TestQueue, user: &str, action_id: &str, id: &str) -> ActionResult {
        process_block_action(true, "U_OWNER", queue, &action(user, action_id, id))
    }

    fn reply(bytes: &TestResponse) -> Value {
        serde_json::from_slice(&bytes.lock().unwrap()).unwrap()
    }

    fn action(user: &str, action_id: &str, id: &str) -> Value {
        let (_, blocks) = build_message_blocks(
            id,
            "title",
            "⏱ 요청시각: 12:30\n👤 요청자: Alice\n▶️ 승인 시 실행: cargo test\n\n## Summary\n**original body**\n\n## 🤖 AI 판단\nreviewed",
            None,
        );
        json!({
            "type": "block_actions",
            "user": { "id": user },
            "actions": [{ "type": "button", "action_id": action_id, "value": id }],
            "channel": { "id": "D123" },
            "container": { "message_ts": "1548261231.000200" },
            "message": { "ts": "1548261231.000200", "blocks": blocks }
        })
    }

    #[test]
    fn token_cache_failure_expires_with_backoff_and_success_is_kept() {
        let cache = TokenCache::default();
        let t0 = Instant::now();
        let reads = std::sync::atomic::AtomicUsize::new(0);
        let fail = || {
            reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err::<(String, String), _>("keychain unavailable".to_string())
        };
        let ok = || {
            reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(("xapp-new".to_string(), "xoxb-new".to_string()))
        };

        assert!(cache.read_at(0, t0, fail).is_err());
        // 1분 안에는 키체인을 다시 읽지 않는다
        assert!(cache.read_at(0, t0 + Duration::from_secs(59), ok).is_err());
        assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 1);
        // 1분이 지나면 다시 읽고, 두 번째 실패는 2분 간격
        let t1 = t0 + Duration::from_secs(60);
        assert!(cache.read_at(0, t1, fail).is_err());
        assert!(cache.read_at(0, t1 + Duration::from_secs(119), ok).is_err());
        assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 2);
        // 복구되면 성공을 캐시한다
        let t2 = t1 + Duration::from_secs(120);
        assert!(cache.read_at(0, t2, ok).is_ok());
        assert!(cache
            .read_at(0, t2 + Duration::from_secs(10_000), || panic!("cached"))
            .is_ok());
        assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 3);
        // 세대가 바뀌면 즉시 다시 읽는다
        assert!(cache.read_at(1, t2, fail).is_err());
        assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 4);
    }

    #[test]
    fn token_retry_delay_doubles_from_one_minute_to_fifteen() {
        let secs: Vec<u64> = (1..=7).map(|n| token_retry_delay(n).as_secs()).collect();
        assert_eq!(secs, [60, 120, 240, 480, 900, 900, 900]);
    }

    #[test]
    fn liveness_pings_every_interval_and_dies_after_silence() {
        let t0 = Instant::now();
        let mut live = Liveness::new(t0);
        assert_eq!(live.probe(t0 + Duration::from_secs(29)), Probe::Idle);
        assert_eq!(live.probe(t0 + Duration::from_secs(30)), Probe::Ping);
        assert_eq!(live.probe(t0 + Duration::from_secs(31)), Probe::Idle);
        assert_eq!(live.probe(t0 + Duration::from_secs(60)), Probe::Ping);
        assert_eq!(live.probe(t0 + Duration::from_secs(89)), Probe::Idle);
        assert_eq!(live.probe(t0 + Duration::from_secs(90)), Probe::Dead);
    }

    #[test]
    fn any_frame_resets_the_silence_clock() {
        let t0 = Instant::now();
        let mut live = Liveness::new(t0);
        live.on_frame(t0 + Duration::from_secs(80));
        assert_ne!(live.probe(t0 + Duration::from_secs(120)), Probe::Dead);
        assert_eq!(live.probe(t0 + Duration::from_secs(170)), Probe::Dead);
    }

    #[test]
    fn disconnect_alert_fires_once_per_outage_after_five_minutes() {
        let t0 = Instant::now();
        let mut health = Health::new();
        health.set(ConnState::Reconnecting, t0);
        assert!(!health.take_alert(t0 + Duration::from_secs(299)));
        // 토큰 없음으로 바뀌어도 같은 끊김으로 센다
        health.set(ConnState::NoToken, t0 + Duration::from_secs(200));
        assert!(health.take_alert(t0 + Duration::from_secs(300)));
        assert!(!health.take_alert(t0 + Duration::from_secs(900)));
        // 복구 뒤 새 끊김은 다시 알린다
        health.set(ConnState::Connected, t0 + Duration::from_secs(1000));
        assert!(!health.take_alert(t0 + Duration::from_secs(5000)));
        health.set(ConnState::Reconnecting, t0 + Duration::from_secs(2000));
        assert!(health.take_alert(t0 + Duration::from_secs(2300)));
    }

    #[test]
    fn token_cache_update_reloads_after_a_token_write() {
        let cache = TokenCache::default();
        let reads = std::sync::atomic::AtomicUsize::new(0);
        let load = || {
            let version = reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok((format!("xapp-{version}"), format!("xoxb-{version}")))
        };

        assert_eq!(
            cache.read(0, load).unwrap(),
            ("xapp-0".into(), "xoxb-0".into())
        );
        assert_eq!(
            cache
                .read(0, || panic!(
                    "cached token pair should avoid another keychain read"
                ))
                .unwrap(),
            ("xapp-0".into(), "xoxb-0".into())
        );
        assert_eq!(
            cache.read(1, load).unwrap(),
            ("xapp-1".into(), "xoxb-1".into())
        );
        cache.update(|| Ok(())).unwrap();
        assert_eq!(
            cache.read(1, load).unwrap(),
            ("xapp-2".into(), "xoxb-2".into())
        );
        assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 3);
    }

    #[test]
    fn slack_config_defaults_disabled_and_fails_closed() {
        let config = config_from_value(&json!({}));
        assert!(!config.enabled);
        assert!(!config.remote_default, "기존 동작 보존: 기본은 꺼짐");
        assert_eq!(config.remote_action_hosts, ["fnfcorp.scalr.io"]);
        assert_eq!(
            config_from_value(&json!({ "slack": { "enabled": "yes" } })),
            SlackConfig::default()
        );
        assert_eq!(config_payload(&json!({}))["enabled"], false);
        assert_eq!(config_payload(&json!({}))["remote_default"], false);
        assert!(config_from_value(&json!({ "slack": { "remote_default": true } })).remote_default);
        assert_eq!(
            config_from_value(&json!({ "slack": { "remote_default": "yes" } })),
            SlackConfig::default(),
            "잘못된 타입은 전체를 기본값으로 되돌린다"
        );
    }

    fn remote_config(enabled: bool, remote_default: bool, hosts: &[&str]) -> SlackConfig {
        SlackConfig {
            enabled,
            owner_user_id: "U_OWNER".into(),
            remote_action_hosts: hosts.iter().map(|host| (*host).into()).collect(),
            remote_default,
        }
    }

    #[test]
    fn remote_target_cases_have_no_shadowing_match() {
        let exact = remote_config(true, false, &["fnfcorp.scalr.io"]);
        let wildcard = remote_config(true, false, &["*.scalr.io"]);
        let none = remote_config(true, false, &[]);
        let target = |flag, url, config: &SlackConfig| {
            is_remote_target(config, true, flag, false, false, false, url)
        };
        let excluded = |enabled, gate, touch, ask| {
            is_remote_target(
                &remote_config(enabled, false, &[]),
                gate,
                true,
                false,
                touch,
                ask,
                None,
            )
        };
        assert!(target(true, None, &none), "explicit flag alone");
        assert!(target(false, Some("https://fnfcorp.scalr.io/run"), &exact));
        assert!(target(false, Some("https://eu.scalr.io/run"), &wildcard));
        assert!(!target(false, Some("https://scalr.io/run"), &wildcard));
        assert!(!target(false, Some("https://example.com"), &exact));
        assert!(!target(false, None, &none), "flag 도 호스트도 없으면 제외");
        assert!(
            !excluded(true, true, true, false),
            "touch id overrides flag"
        );
        assert!(!excluded(true, true, false, true), "ask overrides flag");
        assert!(!excluded(false, true, false, false));
        assert!(!excluded(true, false, false, false));
    }

    #[test]
    fn remote_default_makes_plain_gates_remote_unless_local_only_or_excluded() {
        let on = remote_config(true, true, &[]);
        let off = remote_config(true, false, &[]);
        // (config, gate, explicit, local_only, touch_id, ask, 기대)
        let cases = [
            // remote_default off: 기존 동작 — 플래그·호스트 없는 게이트는 Slack 안 감
            (&off, true, false, false, false, false, false),
            (&off, true, true, false, false, false, true),
            (&off, true, false, true, false, false, false),
            // remote_default on: 플래그 없이도 Slack
            (&on, true, false, false, false, false, true),
            (&on, true, true, false, false, false, true),
            // --local-only 는 remote_default·명시 플래그보다 우선
            (&on, true, false, true, false, false, false),
            (&on, true, true, true, false, false, false),
            // touch id · ask · 비게이트는 remote_default 로도 못 뚫는다
            (&on, true, false, false, true, false, false),
            (&on, true, false, false, false, true, false),
            (&on, false, false, false, false, false, false),
        ];
        for (index, (config, gate, explicit, local_only, touch_id, ask, expected)) in
            cases.into_iter().enumerate()
        {
            assert_eq!(
                is_remote_target(config, gate, explicit, local_only, touch_id, ask, None),
                expected,
                "case {index}"
            );
        }
        let disabled = remote_config(false, true, &[]);
        assert!(
            !is_remote_target(&disabled, true, false, false, false, false, None),
            "Slack 이 꺼져 있으면 remote_default 도 무효"
        );
    }

    #[test]
    fn local_only_also_overrides_allowed_action_url_hosts() {
        let config = remote_config(true, false, &["fnfcorp.scalr.io"]);
        let url = Some("https://fnfcorp.scalr.io/run");
        assert!(is_remote_target(
            &config, true, false, false, false, false, url
        ));
        assert!(!is_remote_target(
            &config, true, false, true, false, false, url
        ));
    }

    #[test]
    fn masks_all_required_secret_shapes_and_quoted_json_keys() {
        let text = concat!(
            "AKIA1234567890ABCDEF ASIA1234567890ABCDEF ",
            "xoxa-1234567890-secret xoxb-1234567890-secret xoxp-1234567890-secret xoxr-1234567890-secret xapp-1-abc123 ",
            "ghp_123456789012345678901234 ghp_123456789012345678901234 github_pat_abcdefghijklmnopqrstuvwxyz1234567890 ",
            "password=hunter2 secret: 'abc' {\"token\":\"inside-json\"} ",
            r#"{"password":"abc\"def"} "#,
            "-----BEGIN PRIVATE KEY-----\npkcs8 bytes\n-----END PRIVATE KEY-----\n",
            "-----BEGIN RSA PRIVATE KEY-----\nsecret bytes\n-----END RSA PRIVATE KEY-----"
        );
        let masked = mask_secrets(text);
        for secret in [
            "AKIA1234567890ABCDEF",
            "ASIA1234567890ABCDEF",
            "xoxa-1234567890-secret",
            "xoxb-1234567890-secret",
            "xoxp-1234567890-secret",
            "xoxr-1234567890-secret",
            "xapp-1-abc123",
            "ghp_123456789012345678901234",
            "github_pat_abcdefghijklmnopqrstuvwxyz1234567890",
            "hunter2",
            "'abc'",
            "inside-json",
            "pkcs8 bytes",
            "secret bytes",
            "abc",
            "def",
        ] {
            assert!(
                !masked.contains(secret),
                "secret remains: {secret} in {masked}"
            );
        }
        assert!(masked.contains("\"token\":\"****\""));
        assert!(masked.contains("\"password\":\"****\""));
    }

    #[test]
    fn masks_aws_bearer_provider_keys_jwt_and_db_url_passwords() {
        let text = concat!(
            "AWS_SECRET_ACCESS_KEY=wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY ",
            "aws_secret_access_key = abcd1234efgh5678 ",
            "--secret-access-key wJalrXUtnFEMIK7MDENGbPxRfiCY ",
            "Authorization: Bearer abc.def-ghi_jkl123 ",
            "sk-ant-api03-abcdefghijklmnop sk-abcdefghijklmnopqrstu ",
            "gho_123456789012345678901234 ghs_123456789012345678901234 ",
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N ",
            "postgres://admin:S3cr3tPass@db.internal:5432/app"
        );
        let masked = mask_secrets(text);
        for secret in [
            "wJalrXUtnFEMI",
            "abcd1234efgh5678",
            "abc.def-ghi_jkl123",
            "sk-ant-api03",
            "sk-abcdefghijklmnopqrstu",
            "gho_1234",
            "ghs_1234",
            "eyJhbGciOiJIUzI1NiJ9",
            "S3cr3tPass",
        ] {
            assert!(
                !masked.contains(secret),
                "secret remains: {secret} in {masked}"
            );
        }
        assert!(masked.contains("postgres://admin:****@db.internal"));
        assert!(masked.contains("Bearer ****"));
    }

    fn ask_payload(multi: bool, count: usize) -> Value {
        let options: Vec<Value> = (0..count)
            .map(|i| json!({ "label": format!("옵션{i}"), "description": "설명" }))
            .collect();
        json!({ "mode": "ask", "questions": { "questions": [
            { "header": "배포", "question": "어느 환경?", "options": options, "multiSelect": multi }
        ] } })
    }

    #[test]
    fn ask_remote_spec_enforces_question_and_option_limits() {
        assert!(ask_remote_spec(&ask_payload(false, 3)).is_some());
        assert!(ask_remote_spec(&ask_payload(true, 3)).is_some());
        assert!(ask_remote_spec(&ask_payload(false, 0)).is_none());
        assert!(ask_remote_spec(&ask_payload(false, 24)).is_some());
        assert!(ask_remote_spec(&ask_payload(false, 25)).is_none());
        assert!(ask_remote_spec(&ask_payload(true, 10)).is_some());
        assert!(ask_remote_spec(&ask_payload(true, 11)).is_none());
        assert_eq!(
            ask_remote_spec(&two_questions()).unwrap().questions.len(),
            2
        );
        let mut five = two_questions();
        let q = five["questions"]["questions"][0].clone();
        five["questions"]["questions"] = json!([q, q, q, q, q]);
        assert!(ask_remote_spec(&five).is_none());
    }

    /// 단일 선택 "배포" + 복수 선택 "기능" 질문 둘.
    fn two_questions() -> Value {
        json!({ "mode": "ask", "title": "t", "questions": { "questions": [
            { "header": "배포", "question": "환경?", "multiSelect": false,
              "options": [{ "label": "dev" }, { "label": "prd" }] },
            { "question": "기능?", "multiSelect": true,
              "options": [{ "label": "a" }, { "label": "b" }, { "label": "c" }] }
        ] } })
    }

    fn ask_queue(payload: Value) -> (TestResponse, TestQueue) {
        let (bytes, queue) = queued(true);
        {
            let mut q = queue.lock().unwrap();
            q[0].kind = "ask".into();
            q[0].payload = payload;
        }
        (bytes, queue)
    }

    fn ask_click(queue: &TestQueue, user: &str, action: Value) -> ActionResult {
        let mut payload = action_payload(user, action);
        payload["trigger_id"] = json!("trig-1");
        process_block_action(true, "U_OWNER", queue, &payload)
    }

    fn action_payload(user: &str, action: Value) -> Value {
        let mut payload = self::action(user, "x", "q-1");
        payload["actions"] = json!([action]);
        payload
    }

    fn btn(action_id: &str, value: &str) -> Value {
        json!({ "type": "button", "action_id": action_id, "value": value })
    }

    fn progress_blocks(result: ActionResult) -> Vec<Value> {
        match result {
            ActionResult::AskUpdate { blocks, .. } => blocks,
            _ => panic!("AskUpdate 가 아니다"),
        }
    }

    fn texts(blocks: &[Value]) -> String {
        serde_json::to_string(blocks).unwrap()
    }

    #[test]
    fn multi_question_blocks_have_per_question_controls_and_other() {
        let spec = ask_remote_spec(&two_questions()).unwrap();
        let (_, blocks) = build_ask_blocks("q-1", "t", &spec, &[], None);
        let actions: Vec<&Value> = blocks.iter().filter(|b| b["type"] == "actions").collect();
        assert_eq!(actions.len(), 3);
        // 질문 1: 버튼 2 + 기타
        let e0 = actions[0]["elements"].as_array().unwrap();
        assert_eq!(e0[0]["action_id"], "knock_ask_q0_o0");
        assert_eq!(e0[1]["value"], "q-1|0|1");
        assert_eq!(e0[2]["action_id"], "knock_ask_other_0");
        // 질문 2: 체크박스 + 기타
        let e1 = actions[1]["elements"].as_array().unwrap();
        assert_eq!(e1[0]["type"], "checkboxes");
        assert_eq!(e1[0]["options"].as_array().unwrap().len(), 3);
        assert_eq!(e1[1]["action_id"], "knock_ask_other_1");
        // 끝: 제출 + 취소
        let last = actions[2]["elements"].as_array().unwrap();
        assert_eq!(last[0]["action_id"], "knock_ask_submit");
        assert_eq!(last[1]["action_id"], "knock_dismiss");
        assert!(blocks.len() <= SLACK_BLOCK_LIMIT);
    }

    #[test]
    fn single_question_single_select_keeps_buttons_and_adds_other_before_cancel() {
        let spec = ask_remote_spec(&ask_payload(false, 2)).unwrap();
        let (_, blocks) = build_ask_blocks("q-1", "t", &spec, &[], None);
        let ids: Vec<&str> = blocks
            .iter()
            .filter(|b| b["type"] == "actions")
            .flat_map(|b| b["elements"].as_array().unwrap())
            .map(|e| e["action_id"].as_str().unwrap())
            .collect();
        assert_eq!(
            ids,
            [
                "knock_ask_opt_0",
                "knock_ask_opt_1",
                "knock_ask_other_0",
                "knock_dismiss"
            ]
        );
    }

    #[test]
    fn multi_question_click_sequence_marks_selection_then_replies_on_submit() {
        let (bytes, queue) = ask_queue(two_questions());
        // 질문 1 선택 → 갱신
        let blocks = progress_blocks(ask_click(
            &queue,
            "U_OWNER",
            btn("knock_ask_q0_o0", "q-1|0|0"),
        ));
        assert!(texts(&blocks).contains("✓ dev"));
        // 재선택 → 마지막 값
        let blocks = progress_blocks(ask_click(
            &queue,
            "U_OWNER",
            btn("knock_ask_q0_o1", "q-1|0|1"),
        ));
        assert!(texts(&blocks).contains("✓ prd") && !texts(&blocks).contains("✓ dev"));
        // 질문 2 체크 (b, a 순서로 와도 오름차순)
        let check = json!({ "type": "checkboxes", "action_id": "knock_ask_chk_1", "block_id": "1|q-1",
            "selected_options": [{ "value": "q-1|1|1" }, { "value": "q-1|1|0" }] });
        let blocks = progress_blocks(ask_click(&queue, "U_OWNER", check));
        assert!(texts(&blocks).contains("initial_options"));
        // 제출
        let result = ask_click(&queue, "U_OWNER", btn("knock_ask_submit", "q-1"));
        assert!(matches!(
            result,
            ActionResult::Applied {
                decision: "approved",
                ..
            }
        ));
        let response = reply(&bytes);
        assert_eq!(response["answers"]["배포"], json!(["prd"]));
        assert_eq!(response["answers"]["기능?"], json!(["a", "b"]));
        assert_eq!(response["via"], "slack");
        // 끝난 요청의 클릭은 무시
        assert!(matches!(
            ask_click(&queue, "U_OWNER", btn("knock_ask_submit", "q-1")),
            ActionResult::Ignored
        ));
    }

    #[test]
    fn all_single_select_questions_reply_when_last_one_is_answered() {
        let mut payload = two_questions();
        payload["questions"]["questions"][1]["multiSelect"] = json!(false);
        let (bytes, queue) = ask_queue(payload);
        assert!(matches!(
            ask_click(&queue, "U_OWNER", btn("knock_ask_q1_o2", "q-1|1|2")),
            ActionResult::AskUpdate { .. }
        ));
        assert!(matches!(
            ask_click(&queue, "U_OWNER", btn("knock_ask_q0_o0", "q-1|0|0")),
            ActionResult::Applied { .. }
        ));
        let response = reply(&bytes);
        assert_eq!(response["answers"]["배포"], json!(["dev"]));
        assert_eq!(response["answers"]["기능?"], json!(["c"]));
    }

    #[test]
    fn submitting_with_an_unanswered_question_is_ignored_with_a_notice() {
        let (bytes, queue) = ask_queue(two_questions());
        ask_click(&queue, "U_OWNER", btn("knock_ask_q0_o0", "q-1|0|0"));
        let blocks = progress_blocks(ask_click(&queue, "U_OWNER", btn("knock_ask_submit", "q-1")));
        assert!(texts(&blocks).contains("제출하지 않았습니다"));
        assert!(bytes.lock().unwrap().is_empty());
        // 체크를 모두 풀어도(빈 selected_options) 제출은 막힌다
        let check = json!({ "type": "checkboxes", "action_id": "knock_ask_chk_1", "block_id": "1|q-1",
            "selected_options": [] });
        ask_click(&queue, "U_OWNER", check);
        let blocks = progress_blocks(ask_click(&queue, "U_OWNER", btn("knock_ask_submit", "q-1")));
        assert!(texts(&blocks).contains("제출하지 않았습니다"));
    }

    #[test]
    fn other_button_opens_modal_with_trigger_and_metadata() {
        let (_, queue) = ask_queue(two_questions());
        let result = ask_click(&queue, "U_OWNER", btn("knock_ask_other_1", "q-1|1"));
        let ActionResult::OpenModal { trigger_id, view } = result else {
            panic!("OpenModal 이 아니다");
        };
        assert_eq!(trigger_id, "trig-1");
        assert_eq!(view["callback_id"], "knock_ask_other");
        assert_eq!(view["blocks"][0]["element"]["type"], "plain_text_input");
        assert_eq!(view["blocks"][0]["element"]["multiline"], true);
        let meta: Value = serde_json::from_str(view["private_metadata"].as_str().unwrap()).unwrap();
        assert_eq!(
            (meta["id"].as_str(), meta["qi"].as_u64()),
            (Some("q-1"), Some(1))
        );
        // value 의 질문 번호가 action_id 와 다르면 무시
        assert!(matches!(
            ask_click(&queue, "U_OWNER", btn("knock_ask_other_1", "q-1|0")),
            ActionResult::Ignored
        ));
    }

    fn view_submission(user: &str, qi: usize, text: &str) -> Value {
        json!({ "type": "view_submission", "user": { "id": user }, "view": {
            "callback_id": "knock_ask_other",
            "private_metadata": json!({ "id": "q-1", "qi": qi, "channel": "D123", "ts": "1.2" }).to_string(),
            "state": { "values": { "other": { "text": { "type": "plain_text_input", "value": text } } } }
        } })
    }

    #[test]
    fn other_text_replaces_single_choice_and_joins_multi_choice_like_the_mac_window() {
        let (bytes, queue) = ask_queue(two_questions());
        ask_click(&queue, "U_OWNER", btn("knock_ask_q0_o0", "q-1|0|0"));
        let check = json!({ "type": "checkboxes", "action_id": "knock_ask_chk_1", "block_id": "1|q-1",
            "selected_options": [{ "value": "q-1|1|2" }] });
        ask_click(&queue, "U_OWNER", check);
        // 단일 선택에 기타 → 선택 대체 (원문 trim 후 그대로, 마스킹은 표시에만)
        let secret = "token ghp_abcdefghijklmnopqrstuvwxyz0123456789";
        let result = process_block_action(
            true,
            "U_OWNER",
            &queue,
            &view_submission("U_OWNER", 0, secret),
        );
        let blocks = progress_blocks(result);
        assert!(!texts(&blocks).contains("ghp_abcdefghijklmnopqrstuvwxyz0123456789"));
        assert!(!texts(&blocks).contains("✓ dev"));
        // 복수 선택에 기타 → 체크와 함께
        process_block_action(
            true,
            "U_OWNER",
            &queue,
            &view_submission("U_OWNER", 1, "  직접\n입력 "),
        );
        let result = ask_click(&queue, "U_OWNER", btn("knock_ask_submit", "q-1"));
        assert!(matches!(result, ActionResult::Applied { .. }));
        let response = reply(&bytes);
        assert_eq!(response["answers"]["배포"], json!([secret]));
        assert_eq!(response["answers"]["기능?"], json!(["c", "직접\n입력"]));
    }

    #[test]
    fn view_submission_from_non_owner_or_empty_text_is_not_applied() {
        let (bytes, queue) = ask_queue(ask_payload(false, 2));
        let result =
            process_block_action(true, "U_OWNER", &queue, &view_submission("U_EVIL", 0, "x"));
        assert!(matches!(
            result,
            ActionResult::Rejected { message: None, .. }
        ));
        let result = process_block_action(
            true,
            "U_OWNER",
            &queue,
            &view_submission("U_OWNER", 0, "   "),
        );
        assert!(matches!(result, ActionResult::Ignored));
        assert!(bytes.lock().unwrap().is_empty());
        // 질문 1개·단일 선택은 기타 하나로 바로 끝난다
        let result = process_block_action(
            true,
            "U_OWNER",
            &queue,
            &view_submission("U_OWNER", 0, "직접"),
        );
        assert!(matches!(result, ActionResult::Applied { .. }));
        assert_eq!(reply(&bytes)["answers"]["배포"], json!(["직접"]));
    }

    #[test]
    fn other_button_from_non_owner_is_rejected() {
        let (_, queue) = ask_queue(two_questions());
        let result = ask_click(&queue, "U_EVIL", btn("knock_ask_other_0", "q-1|0"));
        assert!(matches!(result, ActionResult::Rejected { .. }));
    }

    #[test]
    fn ask_option_click_replies_with_answers_like_the_mac_window() {
        let (bytes, queue) = queued(true);
        {
            let mut q = queue.lock().unwrap();
            q[0].kind = "ask".into();
            q[0].payload = ask_payload(false, 3);
        }
        let result = process_block_action(
            true,
            "U_OWNER",
            &queue,
            &action("U_OWNER", "knock_ask_opt_1", "q-1|1"),
        );
        assert!(matches!(
            result,
            ActionResult::Applied {
                decision: "approved",
                ..
            }
        ));
        let response = reply(&bytes);
        assert_eq!(response["answers"]["배포"], json!(["옵션1"]));
        assert_eq!(response["via"], "slack");
    }

    #[test]
    fn ask_option_click_rejects_index_mismatch_and_non_ask_entries() {
        let (_, queue) = queued(true);
        let result = process_block_action(
            true,
            "U_OWNER",
            &queue,
            &action("U_OWNER", "knock_ask_opt_0", "q-1|0"),
        );
        assert!(matches!(result, ActionResult::Ignored));
        {
            let mut q = queue.lock().unwrap();
            q[0].kind = "ask".into();
            q[0].payload = ask_payload(false, 2);
        }
        let result = process_block_action(
            true,
            "U_OWNER",
            &queue,
            &action("U_OWNER", "knock_ask_opt_1", "q-1|0"),
        );
        assert!(matches!(result, ActionResult::Ignored));
    }

    #[test]
    fn ask_post_blocks_have_option_buttons_and_cancel() {
        let (_, blocks) = build_post_blocks("q-1", "배포", "본문", None, &["a".into(), "b".into()]);
        let elements = blocks.last().unwrap()["elements"]
            .as_array()
            .unwrap()
            .clone();
        assert_eq!(elements.len(), 3);
        assert_eq!(elements[0]["action_id"], "knock_ask_opt_0");
        assert_eq!(elements[0]["value"], "q-1|0");
        assert_eq!(elements[2]["action_id"], "knock_dismiss");
        assert!(blocks.iter().filter(|b| b["type"] == "actions").count() == 1);
    }

    #[test]
    fn approve_all_button_is_third_and_absent_from_single_and_multi_asks() {
        let (_, blocks) = build_message_blocks("q-all", "title", "body", None);
        let elements = blocks.last().unwrap()["elements"].as_array().unwrap();
        assert_eq!(elements[0]["action_id"], "knock_approve");
        assert_eq!(elements[1]["action_id"], "knock_dismiss");
        let button = &elements[2];
        assert_eq!(button["action_id"], "knock_approve_all");
        assert_eq!(button["text"]["type"], "plain_text");
        assert_eq!(button["text"]["text"], "모두 승인");
        assert_eq!(button["value"], "q-all");
        assert!(button.get("style").is_none());

        for spec in [
            ask_remote_spec(&ask_payload(false, 2)).unwrap(),
            ask_remote_spec(&two_questions()).unwrap(),
        ] {
            let (_, ask_blocks) = build_ask_blocks("q-ask", "question", &spec, &[], None);
            assert!(!texts(&ask_blocks).contains("knock_approve_all"));
        }
    }

    #[test]
    fn token_crossing_limit_is_masked_before_unicode_truncation() {
        let value = format!(
            "{} xoxb-1234567890-secret{}",
            "x".repeat(1489),
            "🙂".repeat(200)
        );
        let body = truncate_body(&value, 1500);
        assert!(!body.contains("xoxb-"));
        assert!(body.starts_with(&format!("{} ****", "x".repeat(1489))));
        assert!(body.ends_with("…(맥에서 전체 보기)"));
        assert_eq!(
            body.chars().count(),
            1500 + "…(맥에서 전체 보기)".chars().count()
        );
    }

    #[test]
    fn post_blocks_extract_metadata_execution_and_only_the_ai_section() {
        let body = concat!(
            "⏱ 요청시각: 12:30\n👤 요청자: Alice\n▶️ 승인 시 실행: cargo test\n\n",
            "## 설명\n**본문** [문서](https://example.test)\n\n```md\n## 🤖 AI 판단\n```\n\n",
            "## 🤖 AI 판단\nsecret=hidden\n\n## 다음 단계\n계속"
        );
        let (fallback, blocks) = build_message_blocks("q-7", "deploy.md", body, None);
        let title = blocks[0]["text"]["text"].as_str().unwrap();
        let execution = blocks[2]["text"]["text"].as_str().unwrap();
        let body = blocks[3]["text"]["text"].as_str().unwrap();
        let ai = blocks[4]["text"]["text"].as_str().unwrap();
        assert!(title.contains("⚠️ *deploy.md*"));
        assert_eq!(blocks[1]["elements"][0]["text"], "👤 Alice · ⏱ 12:30");
        assert!(execution.contains("```\ncargo test\n```"));
        assert!(
            body.contains("*본문* <https://example.test|문서>") && body.contains("*다음 단계*")
        );
        assert!(body.contains("```\n## 🤖 AI 판단\n```"));
        assert!(ai.contains("🤖 *AI 판단*\nsecret=****"));
        assert!(!ai.contains("## 🤖"));
        assert!(fallback.contains("cargo test") && fallback.contains("secret=****"));
        assert_eq!(blocks[5]["type"], "actions");
        assert_eq!(blocks[5]["elements"][0]["action_id"], "knock_approve");
        assert_eq!(blocks[5]["elements"][1]["action_id"], "knock_dismiss");
        assert_eq!(blocks[5]["elements"][0]["value"], "q-7");
    }

    #[test]
    fn execution_inline_code_delimiters_are_removed_but_fenced_shell_ticks_remain() {
        let (_, inline) = build_message_blocks(
            "q-inline",
            "title",
            "▶️ 승인 시 실행: `echo noop` (설명)",
            None,
        );
        let inline = inline[1]["text"]["text"].as_str().unwrap();
        assert!(inline.contains("```\necho noop (설명)\n```"), "{inline}");
        assert!(!inline.contains("`echo noop`"), "{inline}");

        let (_, fenced) = build_message_blocks(
            "q-shell",
            "title",
            "▶️ 승인 시 실행:\n```sh\necho `date`\n```",
            None,
        );
        let fenced = fenced[1]["text"]["text"].as_str().unwrap();
        assert!(fenced.contains("```\necho `date`\n```"), "{fenced}");
    }

    #[test]
    fn processed_blocks_keep_original_content_and_replace_buttons_with_one_status_line() {
        let title = "승인이 필요합니다.md";
        let body = "⏱ 요청시각: 12:30\n👤 요청자: Alice\n▶️ 승인 시 실행: cargo test\n\n## 승인이 필요합니다\n**본문 유지**\n\n## 🤖 AI 판단\n위험 없음";
        let (_, mut expected) = build_message_blocks("q-status", title, body, None);
        expected.pop();
        expected[0]["text"]["text"] = Value::String("✅ *승인이 필요합니다.md* 승인됨".into());

        let (integration, receiver) = Integration::new();
        integration.queue_request("q-status".into(), title.into(), body.into(), None);
        assert!(matches!(receiver.try_recv(), Ok(Work::Post { .. })));
        assert!(integration
            .posted(
                "q-status",
                SlackMessageRef {
                    channel: "D1".into(),
                    ts: "1.2".into(),
                },
            )
            .is_none());
        integration.mark_processed("q-status", "approved", "mac");
        match receiver.try_recv() {
            Ok(Work::Update {
                text,
                blocks: Some(blocks),
                ..
            }) => {
                assert_eq!(text, "승인됨");
                let blocks = blocks.as_array().unwrap();
                assert_eq!(blocks.len(), expected.len() + 1);
                assert_eq!(&blocks[..expected.len()], expected.as_slice());
                assert_eq!(blocks.last().unwrap()["type"], "context");
                assert_status_time(
                    blocks.last().unwrap()["elements"][0]["text"]
                        .as_str()
                        .unwrap(),
                    "✅ 맥에서 승인",
                );
            }
            _ => panic!("local decision must update the cached original blocks"),
        }
    }

    #[test]
    fn processed_status_distinguishes_decisions_and_all_sources() {
        for (decision, via, prefix, title) in [
            (
                "approved",
                "slack",
                "✅ Slack에서 승인",
                "✅ *title* 승인됨",
            ),
            ("dismissed", "mac", "✖️ 맥에서 취소", "✖️ *title* 취소됨"),
            ("approved", "ctl", "✅ ctl에서 승인", "✅ *title* 승인됨"),
            ("approved", "auto", "✅ 자동 승인", "✅ *title* 승인됨"),
            ("denied", "slack", "✖️ Slack에서 거절", "✖️ *title* 거절됨"),
            ("annotated", "ctl", "☑️ ctl에서 처리", "☑️ *title* 처리됨"),
        ] {
            let (_, original) = build_message_blocks("q", "title", "본문", None);
            let blocks = processed_message_blocks(Some(&original), decision, via);
            let blocks = blocks.as_array().unwrap();
            assert_eq!(blocks[0]["text"]["text"], title);
            assert!(blocks.iter().all(|block| block["type"] != "actions"));
            let status = blocks.last().unwrap()["elements"][0]["text"]
                .as_str()
                .unwrap();
            assert_status_time(status, prefix);
        }
    }

    #[test]
    fn resolved_actions_prefer_cached_blocks_and_fallback_recognizes_warning_variants() {
        let (tracked, receiver) = Integration::new();
        let payload = action("U_OWNER", "knock_approve", "q-cached");
        tracked.queue_request(
            "q-cached".into(),
            "cached title".into(),
            "**cached body**".into(),
            None,
        );
        assert!(matches!(receiver.try_recv(), Ok(Work::Post { .. })));
        assert!(tracked
            .posted("q-cached", message_ref(&payload).unwrap())
            .is_none());
        tracked.resolve_action("q-cached", &payload, "approved");
        let cached = match receiver.try_recv() {
            Ok(Work::Update {
                blocks: Some(blocks),
                ..
            }) => blocks,
            _ => panic!("tracked action must update with cached original blocks"),
        };
        let cached_blocks = cached.as_array().unwrap();
        assert_eq!(cached_blocks[0]["text"]["text"], "✅ *cached title* 승인됨");
        assert!(cached.to_string().contains("*cached body*"));
        assert!(!cached.to_string().contains("original body"));
        assert!(cached_blocks.iter().all(|block| block["type"] != "actions"));

        let (untracked, receiver) = Integration::new();
        let mut payload = action("U_OWNER", "knock_approve", "q-untracked");
        payload["message"]["blocks"][0]["text"]["text"] =
            Value::String(":warning: *fallback title* 승인이 필요합니다".into());
        untracked.resolve_action("q-untracked", &payload, "approved");
        let fallback = match receiver.try_recv() {
            Ok(Work::Update {
                blocks: Some(blocks),
                ..
            }) => blocks,
            _ => panic!("untracked action must use payload blocks"),
        };
        let fallback = fallback.to_string();
        assert!(fallback.contains("✅ *fallback title* 승인됨"));
        assert!(fallback.contains("original body"));
        assert!(
            processed_title("⚠️ *literal *star* title* 승인이 필요합니다", "approved")
                .is_some_and(|title| title == "✅ *literal *star* title* 승인됨")
        );
    }

    #[test]
    fn missing_original_blocks_fall_back_to_one_status_context() {
        let (untracked, receiver) = Integration::new();
        let mut payload = action("U_OWNER", "knock_dismiss", "q-old");
        payload["message"].as_object_mut().unwrap().remove("blocks");
        untracked.resolve_action("q-old", &payload, "dismissed");
        match receiver.try_recv() {
            Ok(Work::Update {
                text,
                blocks: Some(blocks),
                ..
            }) => {
                assert_eq!(text, "취소됨");
                assert_eq!(blocks.as_array().unwrap().len(), 1);
                assert_status_time(
                    blocks[0]["elements"][0]["text"].as_str().unwrap(),
                    "✖️ Slack에서 취소",
                );
            }
            _ => panic!("untracked action must still send a status-only update"),
        }
    }

    fn assert_status_time(status: &str, prefix: &str) {
        let (actual_prefix, time) = status.rsplit_once(" · ").unwrap();
        assert_eq!(actual_prefix, prefix);
        chrono::NaiveTime::parse_from_str(time, "%H:%M").unwrap();
    }

    #[test]
    fn markdown_converter_keeps_code_and_table_literals() {
        let output = markdown_to_mrkdwn(concat!(
            "# Heading\n**bold** __under__ [docs `literal`](https://example.test?a=1&b=2) `**[x](y)**`\n\n",
            "```sh\n**[x](y)** <tag>한글\n```\n\n| A | B |\n|---|---|\n| **cell**한글 | [link](url) |\n\n**after**"
        ));
        assert!((1..=6).all(|level| {
            markdown_to_mrkdwn(&format!("{} Heading", "#".repeat(level))).contains("*Heading*")
        }));
        assert!(
            output.contains("*bold* _under_ <https://example.test?a=1&amp;b=2|docs `literal`>"),
            "{output}"
        );
        assert!(output.contains("`**[x](y)**`"));
        assert!(output.contains("```\n**[x](y)** &lt;tag&gt;한글\n```"));
        assert!(output.contains("```\n| A | B |\n|---|---|\n| **cell**한글 | [link](url) |\n```"));
        assert!(output.ends_with("*after*"));
        let (fallback, blocks) =
            build_message_blocks("q-plain", "title", "## Summary\n**body**", None);
        assert!(blocks.iter().all(|block| block["type"] != "context"));
        let plain_body = blocks[2]["text"]["text"].as_str().unwrap();
        assert!(plain_body.contains("*Summary*\n*body*"));
        assert!(fallback.contains("*body*"));
    }

    #[test]
    fn inline_emphasis_boundaries_separate_word_characters_only() {
        for (input, expected) in [
            ("한글**강조**입니다", "한글\u{200b}*강조*\u{200b}입니다"),
            ("x**bold**", "x\u{200b}*bold*"),
            ("**bold**x", "*bold*\u{200b}x"),
            ("1**two**2", "1\u{200b}*two*\u{200b}2"),
            ("name__bold__", "name\u{200b}_bold_"),
            ("__bold__name", "_bold_\u{200b}name"),
            ("_기울임_12", "_기울임_\u{200b}12"),
            ("~취소~입니다", "~취소~\u{200b}입니다"),
            ("left _space_ right", "left _space_ right"),
            (".*punct*!", ".*punct*!"),
            ("unpaired *", "unpaired *"),
        ] {
            assert_eq!(markdown_to_mrkdwn(input), expected, "{input}");
        }
    }

    #[test]
    fn markdown_bullets_become_bullets_while_numbers_and_indentation_remain() {
        let output = markdown_to_mrkdwn(
            "- **첫 항목** [문서](url)\n  * 둘째\n\t- 셋째\n1. 첫 번호\n  2. 둘째 번호",
        );
        assert_eq!(
            output,
            "• *첫 항목* <url|문서>\n  • 둘째\n\t• 셋째\n1. 첫 번호\n  2. 둘째 번호"
        );
    }

    #[test]
    fn long_body_ai_and_action_url_obey_slack_limits() {
        let body = format!(
            "secret=hidden\n```sh\n{}\n```\n\n## 🤖 AI 판단\ntoken=xoxb-1234567890-secret",
            "<".repeat(1600)
        );
        let action_url = format!("https://example.test/{}", "x".repeat(5000));
        let (_, blocks) = build_message_blocks("q-long", "title", &body, Some(&action_url));
        let execution = blocks[2]["text"]["text"].as_str().unwrap();
        let ai = blocks[3]["text"]["text"].as_str().unwrap();
        assert!(execution.contains("secret=****"));
        assert!(execution.contains("맥에서 전체 보기"));
        assert_eq!(execution.matches("```").count() % 2, 0);
        assert!(ai.contains("token=****"));
        assert!(blocks.len() <= SLACK_BLOCK_LIMIT);
        assert!(blocks.iter().all(|block| block["text"]["text"]
            .as_str()
            .is_none_or(|text| utf16_len(text) <= SLACK_SECTION_TEXT_LIMIT)));
        let action = blocks[1]["text"]["text"].as_str().unwrap();
        assert!(action.ends_with("|작업 열기>"));
    }

    #[test]
    fn rejected_action_does_not_duplicate_warning_or_exceed_block_limit() {
        let (_, queue) = queued(true);
        let warning = "⛔ 이 요청은 Slack owner만 결정할 수 있습니다";
        let rejected = |payload| match process_block_action(true, "U_OWNER", &queue, payload) {
            ActionResult::Rejected {
                blocks: Some(blocks),
                ..
            } => blocks,
            _ => panic!("rejected"),
        };
        let mut duplicate = action("U_OTHER", "knock_approve", "q-1");
        duplicate["message"]["blocks"] = json!([
            { "type": "context", "elements": [{ "type": "mrkdwn", "text": warning }] }
        ]);
        let blocks = rejected(&duplicate);
        assert_eq!(blocks.as_array().unwrap().len(), 1);
        assert_eq!(blocks[0]["elements"][0]["text"], warning);

        let mut full = action("U_OTHER", "knock_approve", "q-1");
        full["message"]["blocks"] = json!(vec![json!({ "type": "section" }); SLACK_BLOCK_LIMIT]);
        let blocks = rejected(&full);
        assert_eq!(blocks.as_array().unwrap().len(), SLACK_BLOCK_LIMIT);
    }

    #[test]
    fn block_action_requires_owner_and_uses_shared_responder_once() {
        let (bytes, queue) = queued(true);
        let result = click(&queue, "U_OWNER", "knock_approve", "q-1");
        assert!(matches!(
            result,
            ActionResult::Applied {
                decision: "approved",
                ..
            }
        ));
        assert_eq!(
            reply(&bytes),
            json!({ "decision": "approved", "via": "slack" })
        );
        assert!(queue.lock().unwrap().is_empty());
    }

    #[test]
    fn block_action_rejects_other_user_without_resolving_queue() {
        let (bytes, queue) = queued(true);
        let mut expected = action("U_OTHER", "knock_approve", "q-1")["message"]["blocks"]
            .as_array()
            .unwrap()
            .clone();
        expected.push(json!({ "type": "context", "elements": [
            { "type": "mrkdwn", "text": "⛔ 이 요청은 Slack owner만 결정할 수 있습니다" }
        ] }));
        let result = click(&queue, "U_OTHER", "knock_approve", "q-1");
        match result {
            ActionResult::Rejected {
                message: Some(_),
                blocks: Some(blocks),
            } => assert_eq!(blocks, Value::Array(expected)),
            _ => panic!("other-user action should be rejected with preserved buttons"),
        }
        assert!(bytes.lock().unwrap().is_empty());
        assert_eq!(queue.lock().unwrap().len(), 1);
    }

    #[test]
    fn approve_all_approves_only_eligible_remote_gates_and_keeps_local_only() {
        let (remote, remote_bytes) = gate_entry("remote", true, true, false, false);
        let (local, local_bytes) = gate_entry("local", false, true, false, false);
        let (ask, ask_bytes) = queue_entry("ask", "ask", ask_payload(false, 2), true, false);
        let (touch, touch_bytes) = gate_entry("touch", true, true, true, false);
        let (non_gate, non_gate_bytes) = gate_entry("non-gate", false, false, false, false);
        let (progress, progress_bytes) = gate_entry("progress", false, true, false, true);
        let queue = Arc::new(Mutex::new(vec![
            remote, local, ask, touch, non_gate, progress,
        ]));

        let result = process_block_action_with_touch_id(
            true,
            "U_OWNER",
            &queue,
            &action("U_OWNER", "knock_approve_all", "remote"),
            false,
        );

        match result {
            ActionResult::AppliedMany {
                clicked_id,
                ids,
                remote_ids,
            } => {
                assert_eq!(clicked_id, "remote");
                assert_eq!(ids, ["remote"]);
                assert_eq!(remote_ids, ["remote"]);
            }
            _ => panic!("bulk approval should be applied"),
        }
        assert_eq!(
            reply(&remote_bytes),
            json!({ "decision": "approved", "via": "slack" })
        );
        assert!(local_bytes.lock().unwrap().is_empty());
        assert!(ask_bytes.lock().unwrap().is_empty());
        assert!(touch_bytes.lock().unwrap().is_empty());
        assert!(non_gate_bytes.lock().unwrap().is_empty());
        assert!(progress_bytes.lock().unwrap().is_empty());
        assert_eq!(
            queue
                .lock()
                .unwrap()
                .iter()
                .map(|entry| entry.id.as_str())
                .collect::<Vec<_>>(),
            ["local", "ask", "touch", "non-gate", "progress"]
        );
    }

    #[test]
    fn approve_all_rejects_non_owner_without_changing_queue_or_responses() {
        let (bytes, queue) = queued(true);
        let result = process_block_action_with_touch_id(
            true,
            "U_OWNER",
            &queue,
            &action("U_OTHER", "knock_approve_all", "q-1"),
            false,
        );
        assert!(matches!(result, ActionResult::Rejected { .. }));
        assert_eq!(queue.lock().unwrap().len(), 1);
        assert!(bytes.lock().unwrap().is_empty());
    }

    #[test]
    fn approve_all_respects_global_touch_id_setting() {
        let (anchor, anchor_bytes) = gate_entry("anchor", true, true, false, false);
        let (plain, plain_bytes) = gate_entry("plain", false, true, false, false);
        let globally_protected = Arc::new(Mutex::new(vec![anchor, plain]));
        let result = process_block_action_with_touch_id(
            true,
            "U_OWNER",
            &globally_protected,
            &action("U_OWNER", "knock_approve_all", "anchor"),
            true,
        );
        assert!(matches!(result, ActionResult::AppliedMany { ref ids, .. } if ids.is_empty()));
        assert!(anchor_bytes.lock().unwrap().is_empty());
        assert!(plain_bytes.lock().unwrap().is_empty());
        assert_eq!(globally_protected.lock().unwrap().len(), 2);
    }

    #[test]
    fn approve_all_wrapper_uses_current_global_touch_id_setting() {
        let (entry, bytes) = gate_entry("remote", true, true, false, false);
        let queue = Arc::new(Mutex::new(vec![entry]));
        let config_touch_id = crate::config_touch_id();

        let result = process_block_action(
            true,
            "U_OWNER",
            &queue,
            &action("U_OWNER", "knock_approve_all", "remote"),
        );

        match result {
            ActionResult::AppliedMany { ids, .. } if config_touch_id => {
                assert!(ids.is_empty());
                assert_eq!(queue.lock().unwrap().len(), 1);
                assert!(bytes.lock().unwrap().is_empty());
            }
            ActionResult::AppliedMany { ids, .. } => {
                assert_eq!(ids, ["remote"]);
                assert_eq!(reply(&bytes)["via"], "slack");
                assert!(queue.lock().unwrap().is_empty());
            }
            _ => panic!("bulk approval wrapper should be applied"),
        }
    }

    #[test]
    fn approve_all_ignores_stale_anchor_without_resolving_other_requests() {
        let (local, local_bytes) = gate_entry("local", false, true, false, false);
        let queue = Arc::new(Mutex::new(vec![local]));

        for anchor in ["stale", "local"] {
            assert!(matches!(
                process_block_action_with_touch_id(
                    true,
                    "U_OWNER",
                    &queue,
                    &action("U_OWNER", "knock_approve_all", anchor),
                    false
                ),
                ActionResult::Ignored
            ));
        }
        assert_eq!(queue.lock().unwrap().len(), 1);
        assert!(local_bytes.lock().unwrap().is_empty());
    }

    #[test]
    fn stale_or_non_slack_queue_ids_are_ignored() {
        let (bytes, queue) = queued(false);
        let result = click(&queue, "U_OWNER", "knock_dismiss", "q-1");
        assert!(matches!(result, ActionResult::Ignored));
        assert!(bytes.lock().unwrap().is_empty());
        assert!(matches!(
            click(&queue, "U_OWNER", "knock_approve", "stale"),
            ActionResult::Ignored
        ));
    }

    #[test]
    fn dismiss_action_replies_with_slack_via() {
        let (bytes, queue) = queued(true);
        let result = click(&queue, "U_OWNER", "knock_dismiss", "q-1");
        assert!(matches!(
            result,
            ActionResult::Applied {
                decision: "dismissed",
                ..
            }
        ));
        assert_eq!(
            reply(&bytes),
            json!({ "decision": "dismissed", "via": "slack" })
        );
    }

    #[test]
    fn envelope_ack_is_minimal_json_and_backoff_caps() {
        assert_eq!(
            serde_json::from_str::<Value>(&envelope_ack("env-1")).unwrap(),
            json!({ "envelope_id": "env-1" })
        );
        assert_eq!(reconnect_backoff(0), Duration::from_secs(1));
        assert_eq!(reconnect_backoff(1), Duration::from_secs(2));
        assert_eq!(reconnect_backoff(5), Duration::from_secs(32));
        assert_eq!(reconnect_backoff(9), Duration::from_secs(60));
    }

    #[test]
    fn resolving_before_post_response_updates_the_later_message() {
        let (integration, _receiver) = Integration::new();
        integration.queue_request("q-1".into(), "title".into(), "**원문**".into(), None);
        integration.mark_processed("q-1", "approved", "mac");
        let update = integration.posted(
            "q-1",
            SlackMessageRef {
                channel: "D1".into(),
                ts: "1.2".into(),
            },
        );
        match update {
            Some(Work::Update {
                text,
                blocks: Some(blocks),
                ..
            }) => {
                assert_eq!(text, "승인됨");
                assert!(blocks.to_string().contains("✅ 맥에서 승인"));
                assert!(blocks.to_string().contains("*원문*"));
                assert!(!blocks.to_string().contains("actions"));
            }
            _ => panic!("processed request must update the posted original blocks"),
        }

        let (clicked, receiver) = Integration::new();
        clicked.queue_request("q-2".into(), "title".into(), "body".into(), None);
        assert!(matches!(receiver.try_recv(), Ok(Work::Post { .. })));
        clicked.resolve_action(
            "q-2",
            &action("U_OWNER", "knock_approve", "q-2"),
            "approved",
        );
        match receiver.try_recv() {
            Ok(Work::Update {
                message,
                text,
                blocks: Some(blocks),
                ..
            }) => {
                assert_eq!(message.channel, "D123");
                assert_eq!(message.ts, "1548261231.000200");
                assert_eq!(text, "승인됨");
                let serialized = blocks.to_string();
                assert!(serialized.contains("✅ Slack에서 승인"));
                assert!(serialized.contains("body"));
                assert!(!serialized.contains("original body"));
                assert!(serialized.contains("✅ *title* 승인됨"));
                assert!(!serialized.contains("actions"));
            }
            _ => panic!("Slack action must send the processed original blocks"),
        }

        let (unsent, receiver) = Integration::new();
        unsent.queue_request("q-3".into(), "title".into(), "body".into(), None);
        assert!(matches!(receiver.try_recv(), Ok(Work::Post { .. })));
        unsent.mark_processed("q-3", "approved", "ctl");
        assert!(!unsent.should_post_or_forget("q-3"));
        assert!(!unsent.messages.lock().unwrap().contains_key("q-3"));
    }

    #[test]
    fn disabled_worker_gate_never_reads_tokens_or_connects() {
        let config = SlackConfig {
            enabled: false,
            owner_user_id: "U_OWNER".into(),
            remote_action_hosts: vec![],
            remote_default: false,
        };
        let calls = std::cell::Cell::new(0);
        let result = connect_if_enabled(
            &config,
            || {
                calls.set(calls.get() + 1);
                Ok::<_, String>(())
            },
            |_, _| {
                calls.set(calls.get() + 1);
                Ok::<_, String>(())
            },
        );
        assert_eq!(result.unwrap(), None);
        assert_eq!(calls.get(), 0);
        assert!(ensure_test_enabled(&config).is_err());
    }

    #[test]
    fn reconnect_outbox_keeps_posts_and_updates_in_order() {
        let mut pending = VecDeque::new();
        let post = Work::Post {
            id: "q-1".into(),
            title: "t".into(),
            body: "b".into(),
            action_url: None,
            ask: None,
        };
        let update = Work::Update {
            id: "q-1".into(),
            text: "done".into(),
            message: SlackMessageRef {
                channel: "D1".into(),
                ts: "1.2".into(),
            },
            blocks: None,
        };
        let mut executions = 0;
        for work in [post, update] {
            assert!(!dispatch_or_hold(work, false, &mut pending, |_| {
                executions += 1;
                Ok(())
            }));
        }
        assert_eq!(executions, 0, "offline jobs must not call HTTP");
        let mut sent = Vec::new();
        while let Some(work) = pending.pop_front() {
            assert!(!dispatch_or_hold(work, true, &mut pending, |work| {
                sent.push(match work {
                    Work::Post { id, .. } => format!("post:{id}"),
                    Work::Update { id, text, .. } => format!("update:{id}:{text}"),
                    _ => "unexpected".into(),
                });
                Ok(())
            }));
        }
        assert_eq!(sent, ["post:q-1", "update:q-1:done"]);
    }

    #[test]
    fn window_and_slack_resolution_race_replies_through_responder_once() {
        let (bytes, queue) = queued(true);
        let barrier = Arc::new(Barrier::new(3));
        let local_queue = queue.clone();
        let local_barrier = barrier.clone();
        let local = thread::spawn(move || {
            local_barrier.wait();
            crate::reply_queue_entry(
                &local_queue,
                "q-1",
                &json!({ "decision": "approved" }),
                |_| true,
                |_| {},
            )
        });
        let slack_queue = queue.clone();
        let slack_barrier = barrier.clone();
        let slack = thread::spawn(move || {
            slack_barrier.wait();
            process_block_action(
                true,
                "U_OWNER",
                &slack_queue,
                &action("U_OWNER", "knock_approve", "q-1"),
            )
        });
        barrier.wait();
        let _ = local.join().unwrap();
        let _ = slack.join().unwrap();
        let output = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
        assert_eq!(
            output.lines().count(),
            1,
            "one real responder reply: {output:?}"
        );
        assert!(queue.lock().unwrap().is_empty());
    }
}
