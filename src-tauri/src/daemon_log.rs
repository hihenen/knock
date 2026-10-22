use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

const MAX_LOG_BYTES: u64 = 1024 * 1024;
const MAX_BACKUPS: usize = 3;
const MAX_RECORD_BYTES: usize = 8 * 1024;

static LOGGER: OnceLock<Mutex<Option<RotatingLog>>> = OnceLock::new();

#[macro_export]
macro_rules! daemon_log {
    ($($arg:tt)*) => {
        $crate::daemon_log::write(format_args!($($arg)*))
    };
}

pub(crate) fn initialize() {
    let logger = default_log_path().and_then(|path| RotatingLog::open(&path).ok());
    let _ = LOGGER.set(Mutex::new(logger));
}

pub(crate) fn write(args: fmt::Arguments<'_>) {
    let message = args.to_string();
    let Some(logger) = LOGGER.get() else {
        write_stderr(message.as_bytes());
        return;
    };
    let mut logger = match logger.lock() {
        Ok(logger) => logger,
        Err(poisoned) => poisoned.into_inner(),
    };
    let record = format_record(&message);
    if append_record_best_effort(logger.as_mut(), &record) {
        return;
    }
    write_stderr(&record);
}

fn append_record_best_effort(logger: Option<&mut RotatingLog>, record: &[u8]) -> bool {
    logger.is_some_and(|logger| logger.append_record(record).is_ok())
}

fn write_stderr(bytes: &[u8]) {
    let mut stderr = io::stderr().lock();
    let _ = stderr.write_all(bytes);
    if bytes.last() != Some(&b'\n') {
        let _ = stderr.write_all(b"\n");
    }
}

fn default_log_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join("Library/Logs/knock/knock.log"))
}

fn format_record(message: &str) -> Vec<u8> {
    // Mask the complete formatted message before removing line breaks or truncating it.
    let masked = crate::slack::mask_secrets(message);
    let mut sanitized = String::with_capacity(masked.len().min(MAX_RECORD_BYTES));
    for character in masked.chars() {
        if character.is_control() {
            sanitized.push(' ');
        } else {
            sanitized.push(character);
        }
    }

    let byte_limit = MAX_RECORD_BYTES - 1;
    if sanitized.len() > byte_limit {
        let boundary = sanitized
            .char_indices()
            .map(|(index, _)| index)
            .take_while(|index| *index <= byte_limit)
            .last()
            .unwrap_or(0);
        sanitized.truncate(boundary);
    }
    sanitized.push('\n');
    sanitized.into_bytes()
}

struct RotatingLog {
    path: PathBuf,
    lock_file: File,
}

impl RotatingLog {
    fn open(path: &Path) -> io::Result<Self> {
        let parent = path.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "log path has no parent directory",
            )
        })?;
        create_private_directory(parent)?;
        ensure_real_directory(parent)?;
        set_private_directory_permissions(parent)?;
        secure_existing_backups(path)?;
        let lock_file = open_active_file(&lock_path(path))?;
        let _active_file = open_active_file(path)?;
        Ok(Self {
            path: path.to_path_buf(),
            lock_file,
        })
    }

    fn append_record(&mut self, record: &[u8]) -> io::Result<()> {
        if self.lock_file.try_lock().is_err() {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "another daemon is writing the log",
            ));
        }
        let result = self.append_record_locked(record);
        let unlock_result = self.lock_file.unlock();
        result.and(unlock_result)
    }

    fn append_record_locked(&mut self, record: &[u8]) -> io::Result<()> {
        let file = open_active_file(&self.path)?;
        let file_len = file.metadata()?.len();
        drop(file);
        if file_len + record.len() as u64 > MAX_LOG_BYTES {
            self.rotate()?;
        }
        let mut file = open_active_file(&self.path)?;
        file.write_all(record)?;
        file.flush()
    }

    fn rotate(&mut self) -> io::Result<()> {
        for index in (1..=MAX_BACKUPS).rev() {
            let source = backup_path(&self.path, index);
            let destination = backup_path(&self.path, index + 1);
            if index == MAX_BACKUPS {
                reject_symlink_if_present(&source)?;
                match fs::remove_file(&source) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                }
            } else {
                reject_symlink_if_present(&source)?;
                reject_symlink_if_present(&destination)?;
                match fs::rename(&source, &destination) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                }
            }
        }
        let first_backup = backup_path(&self.path, 1);
        reject_symlink_if_present(&self.path)?;
        reject_symlink_if_present(&first_backup)?;
        match fs::rename(&self.path, &first_backup) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let _ = open_active_file(&self.path)?;
        Ok(())
    }
}

fn backup_path(path: &Path, index: usize) -> PathBuf {
    let mut backup = path.as_os_str().to_os_string();
    backup.push(format!(".{index}"));
    PathBuf::from(backup)
}

fn lock_path(path: &Path) -> PathBuf {
    let mut lock = path.as_os_str().to_os_string();
    lock.push(".lock");
    PathBuf::from(lock)
}

fn create_private_directory(path: &Path) -> io::Result<()> {
    if fs::symlink_metadata(path).is_err() {
        let parent = path.parent().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "log directory has no parent")
        })?;
        fs::create_dir_all(parent)?;
        #[cfg(unix)]
        {
            use std::fs::DirBuilder;
            use std::os::unix::fs::DirBuilderExt;
            let mut builder = DirBuilder::new();
            builder.mode(0o700);
            match builder.create(path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }
        #[cfg(not(unix))]
        fs::create_dir(path).or_else(|error| {
            if error.kind() == io::ErrorKind::AlreadyExists {
                Ok(())
            } else {
                Err(error)
            }
        })?;
    }
    Ok(())
}

fn ensure_real_directory(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "log directory must be a real directory",
        ));
    }
    Ok(())
}

fn reject_symlink_if_present(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "refusing to follow a log symlink",
        )),
        Ok(metadata) if metadata.is_file() => Ok(()),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "log path must be a regular file",
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn secure_existing_backups(path: &Path) -> io::Result<()> {
    for index in 1..=MAX_BACKUPS {
        set_private_file_permissions_if_present(&backup_path(path, index))?;
    }
    Ok(())
}

fn set_private_file_permissions_if_present(path: &Path) -> io::Result<()> {
    reject_symlink_if_present(path)?;
    #[cfg(unix)]
    if path.exists() {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn open_active_file(path: &Path) -> io::Result<File> {
    reject_symlink_if_present(path)?;
    let mut options = OpenOptions::new();
    options.create(true).append(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "refusing to write through a non-file log path",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let opened = file.metadata()?;
        if opened.dev() != metadata.dev() || opened.ino() != metadata.ino() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "log path changed while opening",
            ));
        }
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    Ok(file)
}

fn set_private_directory_permissions(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_path_resolves_to_private_user_log_directory_without_writing() {
        let home = std::env::var_os("HOME").expect("test environment has a home directory");
        let expected = PathBuf::from(home).join("Library/Logs/knock/knock.log");
        assert_eq!(default_log_path().unwrap(), expected);
    }

    #[test]
    fn default_path_initialization_and_macro_write_in_child_process() {
        let home = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "daemon_log::tests::default_path_child_writer",
                "--nocapture",
            ])
            .env("HOME", home.path())
            .env("KNOCK_LOG_CHILD_TEST", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );

        let log = home.path().join("Library/Logs/knock/knock.log");
        let text = fs::read_to_string(log).unwrap();
        assert!(text.contains("Authorization: Bearer ****"));
        assert!(text.contains("x-slack-signature: ****"));
        assert!(!text.contains("child-only-token"));
        assert!(!text.contains(&"b".repeat(64)));
    }

    #[test]
    fn default_path_child_writer() {
        if std::env::var_os("KNOCK_LOG_CHILD_TEST").is_none() {
            return;
        }
        initialize();
        crate::daemon_log!(
            "Authorization: Bearer child-only-token x-slack-signature: v0={}",
            "b".repeat(64)
        );
    }

    #[test]
    fn redacts_before_truncating_and_writes_one_utf8_record() {
        let long_token = "t".repeat(MAX_RECORD_BYTES * 3);
        let message = format!(
            "Authorization: Bearer {long_token}\nx-slack-signature: v0={}\nkey=-----BEGIN PRIVATE KEY-----\nsecret-bytes\n-----END PRIVATE KEY-----🙂",
            "a".repeat(64)
        );
        let record = format_record(&message);
        let text = std::str::from_utf8(&record).expect("record stays valid UTF-8");
        assert!(record.len() <= MAX_RECORD_BYTES);
        assert!(text.ends_with('\n'));
        assert_eq!(text.lines().count(), 1);
        assert!(!text.contains(&long_token[..64]));
        assert!(!text.contains(&"a".repeat(64)));
        assert!(!text.contains("secret-bytes"));
        assert!(!text.contains("PRIVATE KEY"));
        let boundary_signature = "c".repeat(64);
        let boundary_message = format!(
            "{} v0={boundary_signature}",
            "x".repeat(MAX_RECORD_BYTES - 20)
        );
        let boundary_text = String::from_utf8(format_record(&boundary_message)).unwrap();
        assert!(boundary_text.contains("v0=****"));
        assert!(!boundary_text.contains("cccccc"));
        for input in [
            r#"{"signature": "json-signature-value"}"#,
            r#"{"x-slack-signature":"json-slack-signature-value"}"#,
            "X-Slack-Signature: header-signature-value",
            "https://example.test/?signature=query-signature-value&sig=query-sig-value",
            "https://example.test/?X-Amz-Signature=query-amz-signature-value",
        ] {
            let text = String::from_utf8(format_record(input)).unwrap();
            for sensitive in [
                "json-signature-value",
                "json-slack-signature-value",
                "header-signature-value",
                "query-signature-value",
                "query-sig-value",
                "query-amz-signature-value",
            ] {
                assert!(!text.contains(sensitive), "unmasked value in {text}");
            }
        }
    }

    #[test]
    fn rotates_active_log_and_keeps_only_three_backups() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("knock.log");
        let mut log = RotatingLog::open(&path).unwrap();
        for index in 0..560 {
            let record = format_record(&format!("record-{index:04}-{}", "🙂".repeat(2500)));
            assert!(append_record_best_effort(Some(&mut log), &record));
        }
        let active_size = fs::metadata(&path).unwrap().len();
        assert!(active_size <= MAX_LOG_BYTES);
        assert!(fs::read_to_string(&path).unwrap().contains("record-0559"));
        let backups: Vec<_> = (1..=MAX_BACKUPS)
            .map(|index| backup_path(&path, index))
            .filter(|path| path.exists())
            .collect();
        assert_eq!(backups.len(), MAX_BACKUPS);
        assert!(backups
            .iter()
            .all(|backup| fs::metadata(backup).unwrap().len() <= MAX_LOG_BYTES));
        assert!(!backup_path(&path, MAX_BACKUPS + 1).exists());
    }

    #[cfg(unix)]
    #[test]
    fn creates_private_directory_and_file_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let app_dir = dir.path().join("Logs/knock");
        let path = app_dir.join("knock.log");
        let _log = RotatingLog::open(&path).unwrap();
        assert_eq!(
            fs::metadata(&app_dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(lock_path(&path)).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[cfg(unix)]
    #[test]
    fn refuses_log_symlink_without_touching_its_target() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("outside.log");
        fs::write(&target, b"untouched").unwrap();
        let log_dir = dir.path().join("Logs/knock");
        fs::create_dir_all(&log_dir).unwrap();
        let path = log_dir.join("knock.log");
        symlink(&target, &path).unwrap();
        assert!(RotatingLog::open(&path).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"untouched");
    }

    #[cfg(unix)]
    #[test]
    fn append_failure_is_best_effort() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("knock.log");
        let mut log = RotatingLog::open(&path).unwrap();
        open_active_file(&path)
            .unwrap()
            .set_len(MAX_LOG_BYTES)
            .unwrap();
        let outside = dir.path().join("outside.log");
        fs::write(&outside, b"untouched").unwrap();
        symlink(&outside, backup_path(&path, MAX_BACKUPS)).unwrap();

        let record = format_record("approval processing continues");
        assert!(!append_record_best_effort(Some(&mut log), &record));
        assert!(!append_record_best_effort(Some(&mut log), &record));
        assert_eq!(fs::metadata(&path).unwrap().len(), MAX_LOG_BYTES);
        assert_eq!(fs::read(outside).unwrap(), b"untouched");
    }
}
