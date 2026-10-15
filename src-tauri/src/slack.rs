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
use tungstenite::{stream::MaybeTlsStream, Message, WebSocket};
use url::Url;

const SERVICE: &str = "knock-slack";
const DEFAULT_HOSTS: &[&str] = &["fnfcorp.scalr.io"];
const MAX_BACKOFF_SECS: u64 = 60;
const SOCKET_POLL: Duration = Duration::from_secs(1);
const API_TIMEOUT: Duration = Duration::from_secs(15);
const SOCKET_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

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
        { "type": "button", "action_id": "knock_dismiss", "text": { "type": "plain_text", "text": "취소", "emoji": true }, "style": "danger", "value": id }
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

/// Slack 으로 보낼 수 있는 `ask` 인지 판정한다: 질문 1개, 단일 선택, 선택지 1~24개(버튼 25개 한도).
/// 보내도 되면 (본문 마크다운, 선택지 라벨들).
pub(super) fn ask_remote_spec(inner: &Value) -> Option<(String, Vec<String>)> {
    let questions = inner.get("questions")?.get("questions")?.as_array()?;
    let [question] = questions.as_slice() else {
        return None;
    };
    if question
        .get("multiSelect")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return None;
    }
    let options = question.get("options")?.as_array()?;
    if options.is_empty() || options.len() > 24 {
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
    Some((body, labels))
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
        /// 비어 있지 않으면 승인/취소 대신 선택지 버튼을 단다 (`ask` 단일 선택 질문).
        options: Vec<String>,
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
        self.queue_post(id, title, body, action_url, Vec::new());
    }

    /// `ask` 단일 선택 질문을 선택지 버튼으로 보낸다.
    pub(super) fn queue_ask_request(
        &self,
        id: String,
        title: String,
        body: String,
        options: Vec<String>,
    ) {
        self.queue_post(id, title, body, None, options);
    }

    fn queue_post(
        &self,
        id: String,
        title: String,
        body: String,
        action_url: Option<String>,
        options: Vec<String>,
    ) {
        let (_, blocks) = build_post_blocks(&id, &title, &body, action_url.as_deref(), &options);
        self.messages
            .lock()
            .unwrap()
            .insert(id.clone(), MessageState::Pending { blocks });
        let _ = self.sender.send(Work::Post {
            id,
            title,
            body,
            action_url,
            options,
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
        security_framework::passwords::set_generic_password(SERVICE, account, token.as_bytes())
            .map_err(|error| format!("macOS 키체인 저장 실패 ({})", error.code()))?;
        bump_token_generation()
    })
}

#[cfg(not(target_os = "macos"))]
fn store_token(_account: &str, _token: &str) -> Result<(), String> {
    token_cache().update(|| Err("Slack 원격 승인은 macOS에서만 사용할 수 있습니다".into()))
}

#[cfg(target_os = "macos")]
fn read_token(account: &str) -> Result<String, String> {
    let bytes = security_framework::passwords::get_generic_password(SERVICE, account)
        .map_err(|error| format!("macOS 키체인 항목을 읽을 수 없습니다 ({})", error.code()))?;
    String::from_utf8(bytes).map_err(|_| "macOS 키체인 토큰 형식이 올바르지 않습니다".into())
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
    tokens: Mutex<Option<(u64, TokenResult)>>,
}

type TokenResult = Result<(String, String), String>;

impl TokenCache {
    fn read(
        &self,
        generation: u64,
        load: impl FnOnce() -> Result<(String, String), String>,
    ) -> Result<(String, String), String> {
        let mut cached = self
            .tokens
            .lock()
            .map_err(|_| "Slack 토큰 캐시를 사용할 수 없습니다".to_string())?;
        if let Some((cached_generation, tokens)) = cached.as_ref() {
            if *cached_generation == generation {
                return tokens.clone();
            }
        }
        let tokens = load();
        *cached = Some((generation, tokens.clone()));
        tokens
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
        options,
    } = work
    else {
        return Err("Slack post 작업이 올바르지 않습니다".into());
    };
    let (fallback, blocks) = build_post_blocks(id, title, body, action_url.as_deref(), options);
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
    Rejected {
        message: Option<SlackMessageRef>,
        blocks: Option<Value>,
    },
    Ignored,
}

pub(super) fn process_block_action(
    enabled: bool,
    owner_user_id: &str,
    queue: &Arc<Mutex<Vec<crate::QueueEntry>>>,
    payload: &Value,
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
    let Some(action) = payload
        .get("actions")
        .and_then(Value::as_array)
        .and_then(|actions| actions.first())
    else {
        eprintln!("knock: Slack action ignored (no actions in payload)");
        return ActionResult::Ignored;
    };
    if let Some(index) = action
        .get("action_id")
        .and_then(Value::as_str)
        .and_then(|action_id| action_id.strip_prefix("knock_ask_opt_"))
        .and_then(|index| index.parse::<usize>().ok())
    {
        return process_ask_option(queue, action, index);
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

/// `ask` 선택지 버튼: 큐 항목의 질문에서 라벨을 찾아 맥 창과 같은 형식의 answers 로 답한다.
fn process_ask_option(
    queue: &Arc<Mutex<Vec<crate::QueueEntry>>>,
    action: &Value,
    index: usize,
) -> ActionResult {
    let Some(value) = action.get("value").and_then(Value::as_str) else {
        eprintln!("knock: Slack action ignored (no value on ask button)");
        return ActionResult::Ignored;
    };
    let Some((id, value_index)) = value.rsplit_once('|') else {
        eprintln!("knock: Slack action ignored (malformed ask button value)");
        return ActionResult::Ignored;
    };
    if value_index.parse::<usize>().ok() != Some(index) {
        eprintln!("knock: Slack action ignored (ask button index mismatch)");
        return ActionResult::Ignored;
    }
    let question = {
        let queue = queue.lock().unwrap();
        queue
            .iter()
            .find(|entry| entry.id == id && entry.remote_slack && entry.kind == "ask")
            .and_then(|entry| ask_remote_spec(&entry.payload).map(|_| entry.payload.clone()))
            .and_then(|payload| payload["questions"]["questions"].get(0).cloned())
    };
    let Some(question) = question else {
        eprintln!("knock: Slack action ignored (no pending remote ask for id={id})");
        return ActionResult::Ignored;
    };
    let Some(label) = question["options"]
        .get(index)
        .and_then(|option| option.get("label"))
        .and_then(Value::as_str)
    else {
        eprintln!("knock: Slack action ignored (ask option {index} out of range)");
        return ActionResult::Ignored;
    };
    let key = question
        .get("header")
        .and_then(Value::as_str)
        .filter(|header| !header.is_empty())
        .or_else(|| {
            question
                .get("question")
                .and_then(Value::as_str)
                .filter(|q| !q.is_empty())
        })
        .unwrap_or("q0");
    let response = json!({ "answers": { key: [label] }, "decision": "approved", "via": "slack" });
    if crate::reply_queue_entry(queue, id, &response, |entry| entry.remote_slack, |_| {}).is_none()
    {
        eprintln!("knock: Slack action ignored (no pending remote entry for id={id})");
        return ActionResult::Ignored;
    }
    ActionResult::Applied {
        id: id.to_string(),
        decision: "approved",
    }
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

fn run_socket_reader(
    mut socket: WebSocket<MaybeTlsStream<TcpStream>>,
    integration: Integration,
    config: SlackConfig,
    generation: u64,
    stop: Arc<AtomicBool>,
) {
    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        match socket.read() {
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
                if envelope.get("type").and_then(Value::as_str) == Some("disconnect") {
                    break;
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
                if payload.get("type").and_then(Value::as_str) == Some("block_actions") {
                    integration.deliver_action(payload, config.clone());
                }
            }
            Ok(Message::Close(_)) => break,
            Ok(_) => {}
            Err(tungstenite::Error::Io(error))
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) => {}
            Err(_) => break,
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
        ActionResult::Rejected {
            message: Some(message),
            blocks,
        } => Some(Work::Update {
            id: String::new(),
            text: "⛔ 이 요청은 Slack owner만 결정할 수 있습니다".into(),
            message,
            blocks,
        }),
        ActionResult::Rejected { message: None, .. } | ActionResult::Ignored => None,
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
        if matches!(&work, Work::Post { .. } | Work::Update { .. }) {
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
                    retry_at = Instant::now() + reconnect_backoff(attempt);
                    attempt = attempt.saturating_add(1);
                }
                Err(StartError::Connection) => {
                    eprintln!("knock: Slack Socket Mode disconnected; reconnecting");
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
                }
            }
        }

        let work = if session.is_some() {
            match receiver.try_recv() {
                Ok(work @ (Work::Action { .. } | Work::SocketEnded { .. })) => Some(work),
                Ok(work @ (Work::Post { .. } | Work::Update { .. })) if !pending.is_empty() => {
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
                    retry_at = Instant::now() + reconnect_backoff(attempt);
                    attempt = attempt.saturating_add(1);
                }
            }
            work @ (Work::Post { .. } | Work::Update { .. }) => {
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
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let queue = Arc::new(Mutex::new(vec![crate::QueueEntry {
            in_progress: false,
            id: "q-1".into(),
            kind: "annotate".into(),
            payload: json!({ "mode": "annotate" }),
            source: json!({}),
            created_at: 1,
            remote_slack,
            decision_via: None,
            responder: crate::ipc::Responder::new(SharedWriter(bytes.clone())),
        }]));
        (bytes, queue)
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
    fn token_cache_reads_once_and_caches_read_errors() {
        let cache = TokenCache::default();
        let reads = std::sync::atomic::AtomicUsize::new(0);
        let load = || {
            reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err("keychain unavailable".to_string())
        };

        assert_eq!(cache.read(0, load), Err("keychain unavailable".into()));
        assert_eq!(
            cache.read(0, || {
                reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(("xapp-new".into(), "xoxb-new".into()))
            }),
            Err("keychain unavailable".into())
        );
        assert_eq!(
            cache.read(1, || {
                reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(("xapp-new".into(), "xoxb-new".into()))
            }),
            Ok(("xapp-new".into(), "xoxb-new".into()))
        );
        assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 2);
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
    fn ask_remote_spec_only_accepts_single_single_select_questions() {
        assert!(ask_remote_spec(&ask_payload(false, 3)).is_some());
        assert!(ask_remote_spec(&ask_payload(true, 3)).is_none());
        assert!(ask_remote_spec(&ask_payload(false, 0)).is_none());
        assert!(ask_remote_spec(&ask_payload(false, 25)).is_none());
        let two = json!({ "questions": { "questions": [
            { "question": "a", "options": [{ "label": "x" }] },
            { "question": "b", "options": [{ "label": "y" }] }
        ] } });
        assert!(ask_remote_spec(&two).is_none());
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
            options: Vec::new(),
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
