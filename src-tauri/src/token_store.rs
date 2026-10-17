//! Slack 토큰 저장소.
//!
//! 기본은 `/usr/bin/security` 서브프로세스로 키체인을 읽고 쓴다. 접근하는 쪽이 Apple 도구라
//! knock 바이너리가 바뀌어도(키체인 파티션 목록의 cdhash) 허용 창이 다시 뜨지 않는다.
//! 키체인을 못 쓰면 `~/.config/knock/slack-tokens.json`(0600)으로 폴백한다.

use serde_json::{json, Map, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const SERVICE: &str = "knock-slack";
const SECURITY_BIN: &str = "/usr/bin/security";
/// `security` 의 errSecItemNotFound 종료 코드.
const EXIT_NOT_FOUND: i32 = 44;
const RUN_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, PartialEq, Eq)]
pub(super) enum SecurityError {
    /// 항목 없음.
    NotFound,
    /// 실행 실패·거부·잠금 등.
    Failed,
}

/// `security` 호출을 추상화한다 (테스트 주입용). 반환값은 stdout 이고 비밀은 로그에 남기지 않는다.
pub(super) trait Security {
    fn read(&self, account: &str) -> Result<String, SecurityError>;
    /// 토큰은 인자가 아니라 표준입력으로만 전달한다.
    fn write(&self, account: &str, token: &str) -> Result<(), SecurityError>;
}

/// 옛 방식(security-framework, knock 만 ACL)으로 만든 항목 접근. 첫 이전 때만 쓴다.
pub(super) trait Legacy {
    fn read(&self, account: &str) -> Option<String>;
    fn delete(&self, account: &str);
}

pub(super) struct SecurityCli {
    /// 지정하면 이 키체인 파일만 쓴다 (테스트 전용 임시 키체인).
    pub keychain: Option<PathBuf>,
}

impl SecurityCli {
    fn run(
        &self,
        args: &[&str],
        stdin: Option<&str>,
    ) -> Result<std::process::Output, SecurityError> {
        let mut command = Command::new(SECURITY_BIN);
        command
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command.stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
        let mut child = command.spawn().map_err(|_| SecurityError::Failed)?;
        if let (Some(input), Some(mut pipe)) = (stdin, child.stdin.take()) {
            pipe.write_all(input.as_bytes())
                .map_err(|_| SecurityError::Failed)?;
        }
        // 허용 창 같은 대화상자에서 멈추면 설정 저장이 영원히 막히므로 시간 제한을 둔다.
        let deadline = Instant::now() + RUN_TIMEOUT;
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(50))
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(SecurityError::Failed);
                }
            }
        }
        child.wait_with_output().map_err(|_| SecurityError::Failed)
    }

    fn keychain_arg(&self) -> Option<String> {
        self.keychain
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned())
    }
}

impl Security for SecurityCli {
    fn read(&self, account: &str) -> Result<String, SecurityError> {
        let mut args = vec!["find-generic-password", "-s", SERVICE, "-a", account, "-w"];
        let keychain = self.keychain_arg();
        if let Some(path) = &keychain {
            args.push(path);
        }
        let output = self.run(&args, None)?;
        match output.status.code() {
            Some(0) => {
                let text = String::from_utf8(output.stdout).map_err(|_| SecurityError::Failed)?;
                Ok(text.trim_end_matches(['\n', '\r']).to_string())
            }
            Some(EXIT_NOT_FOUND) => Err(SecurityError::NotFound),
            _ => Err(SecurityError::Failed),
        }
    }

    fn write(&self, account: &str, token: &str) -> Result<(), SecurityError> {
        // `security -i` 는 stdin 한 줄을 명령으로 받는다. 따옴표·공백이 든 값은 파싱이 달라지므로 거부한다.
        if token.is_empty()
            || token
                .chars()
                .any(|c| c.is_whitespace() || "\"'\\".contains(c))
        {
            return Err(SecurityError::Failed);
        }
        let keychain = self
            .keychain_arg()
            .map(|path| format!(" \"{path}\""))
            .unwrap_or_default();
        // `-U` 는 기존 항목 ACL 확인 창에서 멈추므로 지우고 새로 만든다 (새 항목은 -T 로 security 를 허용).
        let mut delete = vec!["delete-generic-password", "-s", SERVICE, "-a", account];
        let keychain_path = self.keychain_arg();
        if let Some(path) = &keychain_path {
            delete.push(path);
        }
        let _ = self.run(&delete, None);
        let line = format!(
            "add-generic-password -s {SERVICE} -a {account} -T {SECURITY_BIN} -w {token}{keychain}\n"
        );
        let output = self.run(&["-i"], Some(&line))?;
        // -i 모드는 명령이 실패해도 종료 코드가 0 일 수 있어 stderr 도 본다.
        if output.status.success() && output.stderr.is_empty() {
            Ok(())
        } else {
            Err(SecurityError::Failed)
        }
    }
}

#[cfg(target_os = "macos")]
pub(super) struct FrameworkLegacy;

#[cfg(target_os = "macos")]
impl Legacy for FrameworkLegacy {
    fn read(&self, account: &str) -> Option<String> {
        let bytes = security_framework::passwords::get_generic_password(SERVICE, account).ok()?;
        String::from_utf8(bytes).ok()
    }

    fn delete(&self, account: &str) {
        let _ = security_framework::passwords::delete_generic_password(SERVICE, account);
    }
}

// ---------- 폴백 파일 ----------

pub(super) fn token_file_path() -> PathBuf {
    let base = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(base).join(".config/knock/slack-tokens.json")
}

fn read_file_map(path: &Path) -> Map<String, Value> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default()
}

pub(super) fn file_read(path: &Path, account: &str) -> Option<String> {
    read_file_map(path)
        .get(account)
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// `token` 이 None 이면 해당 계정을 지운다. 디렉토리 0700, 파일 0600, 임시 파일 → rename.
pub(super) fn file_update(path: &Path, account: &str, token: Option<&str>) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent).map_err(|_| "토큰 폴백 디렉토리를 만들 수 없습니다")?;
    let mut map = read_file_map(path);
    match token {
        Some(value) => {
            map.insert(account.to_string(), json!(value));
        }
        None => {
            map.remove(account);
        }
    }
    let mut temp =
        tempfile::NamedTempFile::new_in(parent).map_err(|_| "토큰 폴백 파일을 만들 수 없습니다")?;
    temp.as_file()
        .set_permissions(std::fs::Permissions::from_mode(0o600))
        .map_err(|_| "토큰 폴백 파일 권한을 설정할 수 없습니다")?;
    temp.write_all(Value::Object(map).to_string().as_bytes())
        .and_then(|_| temp.as_file().sync_all())
        .map_err(|_| "토큰 폴백 파일을 쓸 수 없습니다")?;
    temp.persist(path)
        .map_err(|_| "토큰 폴백 파일을 저장할 수 없습니다")?;
    // 0700 은 새로 만든 knock 디렉토리일 때만 좁힌다 (기존 ~/.config 등은 건드리지 않음).
    if parent.file_name().is_some_and(|name| name == "knock") {
        let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
    }
    Ok(())
}

// ---------- 정책 ----------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Mode {
    Keychain,
    File,
}

impl Mode {
    pub(super) fn from_config(config: &Value) -> Mode {
        match config["token_store"].as_str() {
            Some("file") => Mode::File,
            _ => Mode::Keychain,
        }
    }
}

pub(super) fn store(
    mode: Mode,
    security: &dyn Security,
    legacy: &dyn Legacy,
    file: &Path,
    account: &str,
    token: &str,
) -> Result<(), String> {
    if mode == Mode::Keychain {
        // 옛 방식 항목이 남아 있으면 security 가 지우지 못해 쓰기가 실패하고, 옛 값이 읽히는 일이 생긴다.
        let written = security.write(account, token).is_ok() || {
            legacy.delete(account);
            security.write(account, token).is_ok()
        };
        if written {
            // 폴백 파일에 남은 옛 값이 키체인 값을 가리지 않게 지운다.
            let _ = file_update(file, account, None);
            return Ok(());
        }
        eprintln!("knock: 키체인 저장 실패, 토큰을 폴백 파일에 저장합니다 ({account})");
    }
    file_update(file, account, Some(token))
}

pub(super) fn load(
    mode: Mode,
    security: &dyn Security,
    legacy: &dyn Legacy,
    file: &Path,
    account: &str,
) -> Result<String, String> {
    if mode == Mode::File {
        return file_read(file, account).ok_or_else(|| "저장된 토큰이 없습니다".to_string());
    }
    if let Ok(token) = security.read(account) {
        return Ok(token);
    }
    if let Some(token) = file_read(file, account) {
        return Ok(token);
    }
    // 이전 방식 항목: 한 번 읽고(창이 한 번 뜰 수 있다) 곧바로 security 방식으로 다시 저장한다.
    let token = legacy
        .read(account)
        .ok_or_else(|| "macOS 키체인 항목을 읽을 수 없습니다".to_string())?;
    legacy.delete(account);
    if security.write(account, &token).is_err() {
        eprintln!("knock: 키체인 이전 실패, 토큰을 폴백 파일에 저장합니다 ({account})");
        file_update(file, account, Some(&token))?;
    }
    Ok(token)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::os::unix::fs::PermissionsExt;

    #[derive(Default)]
    struct FakeSecurity {
        items: RefCell<HashMap<String, String>>,
        fail_write: bool,
        denied: bool,
        writes: RefCell<Vec<(String, String)>>,
    }

    impl Security for FakeSecurity {
        fn read(&self, account: &str) -> Result<String, SecurityError> {
            if self.denied {
                return Err(SecurityError::Failed);
            }
            self.items
                .borrow()
                .get(account)
                .cloned()
                .ok_or(SecurityError::NotFound)
        }
        fn write(&self, account: &str, token: &str) -> Result<(), SecurityError> {
            if self.fail_write {
                return Err(SecurityError::Failed);
            }
            self.writes
                .borrow_mut()
                .push((account.to_string(), token.to_string()));
            self.items
                .borrow_mut()
                .insert(account.to_string(), token.to_string());
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeLegacy {
        items: RefCell<HashMap<String, String>>,
        reads: RefCell<u32>,
    }

    impl Legacy for FakeLegacy {
        fn read(&self, account: &str) -> Option<String> {
            *self.reads.borrow_mut() += 1;
            self.items.borrow().get(account).cloned()
        }
        fn delete(&self, account: &str) {
            self.items.borrow_mut().remove(account);
        }
    }

    fn file_path(dir: &tempfile::TempDir) -> PathBuf {
        dir.path().join("knock/slack-tokens.json")
    }

    #[test]
    fn security_write_rejects_unsafe_tokens_before_spawning() {
        let cli = SecurityCli { keychain: None };
        assert_eq!(cli.write("a", "xoxb 1"), Err(SecurityError::Failed));
        assert_eq!(cli.write("a", "x\"y"), Err(SecurityError::Failed));
        assert_eq!(cli.write("a", ""), Err(SecurityError::Failed));
    }

    #[test]
    fn store_prefers_keychain_and_clears_stale_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = file_path(&dir);
        file_update(&path, "app_token", Some("old")).unwrap();
        let security = FakeSecurity::default();
        store(
            Mode::Keychain,
            &security,
            &FakeLegacy::default(),
            &path,
            "app_token",
            "xapp-1",
        )
        .unwrap();
        assert_eq!(security.read("app_token").unwrap(), "xapp-1");
        assert_eq!(file_read(&path, "app_token"), None);
    }

    #[test]
    fn store_falls_back_to_file_when_keychain_fails() {
        let dir = tempfile::tempdir().unwrap();
        let path = file_path(&dir);
        let security = FakeSecurity {
            fail_write: true,
            ..Default::default()
        };
        store(
            Mode::Keychain,
            &security,
            &FakeLegacy::default(),
            &path,
            "bot_token",
            "xoxb-1",
        )
        .unwrap();
        assert_eq!(file_read(&path, "bot_token").as_deref(), Some("xoxb-1"));
    }

    #[test]
    fn file_mode_never_touches_keychain() {
        let dir = tempfile::tempdir().unwrap();
        let path = file_path(&dir);
        let security = FakeSecurity::default();
        let legacy = FakeLegacy::default();
        store(Mode::File, &security, &legacy, &path, "app_token", "xapp-2").unwrap();
        assert!(security.writes.borrow().is_empty());
        assert_eq!(
            load(Mode::File, &security, &legacy, &path, "app_token").unwrap(),
            "xapp-2"
        );
        assert_eq!(*legacy.reads.borrow(), 0);
    }

    #[test]
    fn load_reads_security_first_without_legacy() {
        let dir = tempfile::tempdir().unwrap();
        let security = FakeSecurity::default();
        security
            .items
            .borrow_mut()
            .insert("app_token".into(), "xapp-3".into());
        let legacy = FakeLegacy::default();
        let token = load(
            Mode::Keychain,
            &security,
            &legacy,
            &file_path(&dir),
            "app_token",
        );
        assert_eq!(token.unwrap(), "xapp-3");
        assert_eq!(*legacy.reads.borrow(), 0);
    }

    #[test]
    fn load_migrates_legacy_item_then_reads_via_security() {
        let dir = tempfile::tempdir().unwrap();
        let path = file_path(&dir);
        let security = FakeSecurity::default();
        let legacy = FakeLegacy::default();
        legacy
            .items
            .borrow_mut()
            .insert("app_token".into(), "xapp-4".into());
        assert_eq!(
            load(Mode::Keychain, &security, &legacy, &path, "app_token").unwrap(),
            "xapp-4"
        );
        assert!(legacy.items.borrow().is_empty());
        assert_eq!(security.read("app_token").unwrap(), "xapp-4");
        // 두 번째부터는 legacy 를 읽지 않는다.
        load(Mode::Keychain, &security, &legacy, &path, "app_token").unwrap();
        assert_eq!(*legacy.reads.borrow(), 1);
    }

    #[test]
    fn load_migration_keeps_token_in_file_when_rewrite_fails() {
        let dir = tempfile::tempdir().unwrap();
        let path = file_path(&dir);
        let security = FakeSecurity {
            fail_write: true,
            ..Default::default()
        };
        let legacy = FakeLegacy::default();
        legacy
            .items
            .borrow_mut()
            .insert("bot_token".into(), "xoxb-4".into());
        load(Mode::Keychain, &security, &legacy, &path, "bot_token").unwrap();
        assert_eq!(file_read(&path, "bot_token").as_deref(), Some("xoxb-4"));
    }

    #[test]
    fn load_fails_when_nowhere_has_the_token() {
        let dir = tempfile::tempdir().unwrap();
        let security = FakeSecurity {
            denied: true,
            ..Default::default()
        };
        let legacy = FakeLegacy::default();
        assert!(load(
            Mode::Keychain,
            &security,
            &legacy,
            &file_path(&dir),
            "app_token"
        )
        .is_err());
    }

    #[test]
    fn fallback_file_is_private_and_atomic() {
        let dir = tempfile::tempdir().unwrap();
        let path = file_path(&dir);
        file_update(&path, "app_token", Some("a")).unwrap();
        file_update(&path, "bot_token", Some("b")).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(file_read(&path, "app_token").as_deref(), Some("a"));
        // 임시 파일이 남지 않는다.
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1
        );
    }

    #[test]
    fn mode_defaults_to_keychain() {
        assert_eq!(Mode::from_config(&json!({})), Mode::Keychain);
        assert_eq!(
            Mode::from_config(&json!({"token_store": "file"})),
            Mode::File
        );
        assert_eq!(
            Mode::from_config(&json!({"token_store": "x"})),
            Mode::Keychain
        );
    }

    /// 임시 키체인에서만 도는 실측. `cargo test -- --ignored` 로 실행한다.
    #[test]
    #[ignore]
    fn real_security_roundtrip_on_temp_keychain() {
        let dir = tempfile::tempdir().unwrap();
        let keychain = dir.path().join("knock-test.keychain-db");
        let kc = keychain.to_string_lossy().to_string();
        let sh = |args: &[&str]| {
            Command::new(SECURITY_BIN)
                .args(args)
                .status()
                .unwrap()
                .success()
        };
        assert!(sh(&["create-keychain", "-p", "pw", &kc]));
        assert!(sh(&["unlock-keychain", "-p", "pw", &kc]));
        let cli = SecurityCli {
            keychain: Some(keychain.clone()),
        };
        assert_eq!(cli.read("app_token"), Err(SecurityError::NotFound));
        cli.write("app_token", "xapp-1-SECRET-abc").unwrap();
        cli.write("app_token", "xapp-1-SECRET-def").unwrap();
        assert_eq!(cli.read("app_token").unwrap(), "xapp-1-SECRET-def");
        assert!(sh(&["delete-keychain", &kc]));
    }
}
