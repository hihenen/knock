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
}

impl Default for SlackConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            owner_user_id: String::new(),
            remote_action_hosts: DEFAULT_HOSTS.iter().map(|host| (*host).into()).collect(),
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
    enabled: bool,
    gate: bool,
    explicit: bool,
    touch_id: bool,
    ask: bool,
    action_url: Option<&str>,
    hosts: &[String],
) -> bool {
    if !enabled || !gate || touch_id || ask {
        return false;
    }
    if explicit {
        return true;
    }
    let Some(host) = action_url
        .and_then(|url| Url::parse(url).ok())
        .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
    else {
        return false;
    };
    hosts.iter().any(|pattern| {
        if let Some(suffix) = pattern.strip_prefix("*.") {
            host.ends_with(&format!(".{suffix}"))
        } else {
            host == pattern.to_ascii_lowercase()
        }
    })
}

static MASKING_PATTERNS: OnceLock<[Regex; 5]> = OnceLock::new();

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
    patterns[4]
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
        .into_owned()
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
    Pending,
    Posted(Vec<SlackMessageRef>),
    Processed {
        text: String,
        messages: Vec<SlackMessageRef>,
    },
}

pub(super) enum Work {
    Post {
        id: String,
        title: String,
        body: String,
        action_url: Option<String>,
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
        self.messages
            .lock()
            .unwrap()
            .insert(id.clone(), MessageState::Pending);
        let _ = self.sender.send(Work::Post {
            id,
            title,
            body,
            action_url,
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

    pub(super) fn mark_processed(&self, id: &str, text: String) {
        let update = {
            let mut messages = self.messages.lock().unwrap();
            let Some(state) = messages.get_mut(id) else {
                return;
            };
            match state {
                MessageState::Pending => {
                    *state = MessageState::Processed {
                        text: text.clone(),
                        messages: Vec::new(),
                    };
                    Vec::new()
                }
                MessageState::Posted(posted) => {
                    let posted = posted.clone();
                    *state = MessageState::Processed {
                        text: text.clone(),
                        messages: posted.clone(),
                    };
                    posted
                }
                MessageState::Processed { .. } => Vec::new(),
            }
        };
        for message in update {
            let _ = self.sender.send(Work::Update {
                id: id.to_string(),
                text: text.clone(),
                message,
                blocks: None,
            });
        }
    }

    fn should_post(&self, id: &str) -> bool {
        matches!(
            self.messages.lock().unwrap().get(id),
            Some(MessageState::Pending)
        )
    }

    fn should_post_or_forget(&self, id: &str) -> bool {
        let pending = self.should_post(id);
        if !pending {
            self.forget(id);
        }
        pending
    }

    fn resolve_action(&self, id: &str, payload: &Value, text: String) {
        if let Some(update) = message_ref(payload).and_then(|message| self.posted(id, message)) {
            self.enqueue(update);
        }
        self.mark_processed(id, text);
    }

    fn posted(&self, id: &str, message: SlackMessageRef) -> Option<Work> {
        let mut messages = self.messages.lock().unwrap();
        let state = messages.get_mut(id)?;
        match state {
            MessageState::Pending => {
                *state = MessageState::Posted(vec![message]);
                None
            }
            MessageState::Posted(posted) => {
                if !posted.contains(&message) {
                    posted.push(message);
                }
                None
            }
            MessageState::Processed {
                text,
                messages: posted,
            } => {
                if posted.contains(&message) {
                    return None;
                }
                posted.push(message.clone());
                Some(Work::Update {
                    id: id.to_string(),
                    text: text.clone(),
                    message,
                    blocks: None,
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
    security_framework::passwords::set_generic_password(SERVICE, account, token.as_bytes())
        .map_err(|error| format!("macOS 키체인 저장 실패 ({})", error.code()))
}

#[cfg(not(target_os = "macos"))]
fn store_token(_account: &str, _token: &str) -> Result<(), String> {
    Err("Slack 원격 승인은 macOS에서만 사용할 수 있습니다".into())
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

fn read_tokens() -> Result<(String, String), String> {
    let app = read_token("app_token")?;
    let bot = read_token("bot_token")?;
    validate_tokens(&app, &bot)?;
    Ok((app, bot))
}

#[tauri::command]
pub(super) async fn save_slack(
    enabled: bool,
    owner_user_id: String,
    remote_action_hosts: Vec<String>,
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
    } = work
    else {
        return Err("Slack post 작업이 올바르지 않습니다".into());
    };
    let title: String = mask_secrets(title).chars().take(150).collect();
    let title = if title.trim().is_empty() {
        "Knock 승인 요청".to_string()
    } else {
        title
    };
    let body = truncate_body(body, 1500);
    let body = if body.trim().is_empty() {
        "(본문 없음)".to_string()
    } else {
        body
    };
    let action_url = action_url.as_deref().map(mask_secrets);
    let execution = match action_url.as_deref() {
        Some(url) => format!(
            "▶️ 승인 시 실행\n<{}|작업 열기>",
            url.replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
        ),
        None => "▶️ 승인 시 실행".to_string(),
    };
    let fallback = format!("{title}\n{body}\n{}", mask_secrets(&execution));
    let blocks = vec![
        json!({ "type": "header", "text": { "type": "plain_text", "text": title, "emoji": true } }),
        json!({ "type": "section", "text": { "type": "plain_text", "text": body, "emoji": true } }),
        json!({ "type": "section", "text": { "type": "mrkdwn", "text": mask_secrets(&execution) } }),
        json!({ "type": "actions", "elements": [
            { "type": "button", "action_id": "knock_approve", "text": { "type": "plain_text", "text": "승인", "emoji": true }, "style": "primary", "value": id },
            { "type": "button", "action_id": "knock_dismiss", "text": { "type": "plain_text", "text": "취소", "emoji": true }, "style": "danger", "value": id }
        ] }),
    ];
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
        return ActionResult::Ignored;
    }
    if payload
        .get("user")
        .and_then(|user| user.get("id"))
        .and_then(Value::as_str)
        != Some(owner_user_id)
    {
        let blocks = payload
            .get("message")
            .and_then(|message| message.get("blocks"))
            .and_then(Value::as_array)
            .map(|blocks| {
                let mut blocks = blocks.clone();
                blocks.push(json!({ "type": "context", "elements": [
                    { "type": "mrkdwn", "text": "⛔ 이 요청은 Slack owner만 결정할 수 있습니다" }
                ] }));
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
        return ActionResult::Ignored;
    };
    let decision = match action.get("action_id").and_then(Value::as_str) {
        Some("knock_approve") => "approved",
        Some("knock_dismiss") => "dismissed",
        _ => return ActionResult::Ignored,
    };
    let Some(id) = action.get("value").and_then(Value::as_str) else {
        return ActionResult::Ignored;
    };
    let response = json!({ "decision": decision, "via": "slack" });
    if crate::reply_queue_entry(queue, id, &response, |entry| entry.remote_slack, |_| {}).is_none()
    {
        return ActionResult::Ignored;
    }
    ActionResult::Applied {
        id: id.to_string(),
        decision,
    }
}

pub(super) fn status_text(decision: &str) -> String {
    let verb = if decision == "approved" {
        "승인"
    } else {
        "취소"
    };
    format!(
        "✅ Slack에서 {verb} · {}",
        chrono::Local::now().format("%H:%M")
    )
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
            integration.resolve_action(&id, &payload, status_text(decision));
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
        json!({
            "type": "block_actions",
            "user": { "id": user },
            "actions": [{ "type": "button", "action_id": action_id, "value": id }],
            "channel": { "id": "D123" },
            "container": { "message_ts": "1548261231.000200" },
            "message": { "ts": "1548261231.000200", "blocks": [
                { "type": "actions", "elements": [
                    { "type": "button", "action_id": "knock_approve", "value": id }
                ] }
            ] }
        })
    }

    #[test]
    fn slack_config_defaults_disabled_and_fails_closed() {
        let config = config_from_value(&json!({}));
        assert!(!config.enabled);
        assert_eq!(config.remote_action_hosts, ["fnfcorp.scalr.io"]);
        assert_eq!(
            config_from_value(&json!({ "slack": { "enabled": "yes" } })),
            SlackConfig::default()
        );
        assert_eq!(config_payload(&json!({}))["enabled"], false);
    }

    #[test]
    fn remote_target_cases_have_no_shadowing_match() {
        let exact = ["fnfcorp.scalr.io".into()];
        let wildcard = ["*.scalr.io".into()];
        let target = |flag, url, hosts: &[String]| {
            is_remote_target(true, true, flag, false, false, url, hosts)
        };
        let excluded = |enabled, gate, touch, ask| {
            is_remote_target(enabled, gate, true, touch, ask, None, &[])
        };
        assert!(target(true, None, &[]), "explicit flag alone");
        assert!(target(false, Some("https://fnfcorp.scalr.io/run"), &exact));
        assert!(target(false, Some("https://eu.scalr.io/run"), &wildcard));
        assert!(!target(false, Some("https://scalr.io/run"), &wildcard));
        assert!(!target(false, Some("https://example.com"), &exact));
        assert!(
            !excluded(true, true, true, false),
            "touch id overrides flag"
        );
        assert!(!excluded(true, true, false, true), "ask overrides flag");
        assert!(!excluded(false, true, false, false));
        assert!(!excluded(true, false, false, false));
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
        let result = click(&queue, "U_OTHER", "knock_approve", "q-1");
        match result {
            ActionResult::Rejected {
                message: Some(_),
                blocks: Some(blocks),
            } => {
                assert_eq!(blocks[0]["type"], "actions");
                assert_eq!(blocks[0]["elements"][0]["action_id"], "knock_approve");
                assert_eq!(blocks[1]["type"], "context");
            }
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
        integration.queue_request("q-1".into(), "title".into(), "body".into(), None);
        integration.mark_processed("q-1", "처리됨 (맥에서)".into());
        let update = integration.posted(
            "q-1",
            SlackMessageRef {
                channel: "D1".into(),
                ts: "1.2".into(),
            },
        );
        assert!(matches!(update, Some(Work::Update { text, .. }) if text == "처리됨 (맥에서)"));

        let (clicked, receiver) = Integration::new();
        clicked.queue_request("q-2".into(), "title".into(), "body".into(), None);
        assert!(matches!(receiver.try_recv(), Ok(Work::Post { .. })));
        clicked.resolve_action(
            "q-2",
            &action("U_OWNER", "knock_approve", "q-2"),
            "approved".into(),
        );
        assert!(
            matches!(receiver.try_recv(), Ok(Work::Update { message, text, .. })
            if message.channel == "D123" && message.ts == "1548261231.000200" && text == "approved")
        );

        let (unsent, receiver) = Integration::new();
        unsent.queue_request("q-3".into(), "title".into(), "body".into(), None);
        assert!(matches!(receiver.try_recv(), Ok(Work::Post { .. })));
        unsent.mark_processed("q-3", "local".into());
        assert!(!unsent.should_post_or_forget("q-3"));
        assert!(!unsent.messages.lock().unwrap().contains_key("q-3"));
    }

    #[test]
    fn disabled_worker_gate_never_reads_tokens_or_connects() {
        let config = SlackConfig {
            enabled: false,
            owner_user_id: "U_OWNER".into(),
            remote_action_hosts: vec![],
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
