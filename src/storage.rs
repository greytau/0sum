//! Local persistence (PHASE2 §6): `$XDG_CONFIG_HOME/0sum` or `~/.0sum`, one
//! JSON file per session plus an advisory lock file. Modes are hygiene against
//! other unix users only; players sharing a uid read everything, which is why
//! nothing secret is ever stored.

use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use crate::engine::escape;
use crate::engine::hw::HighWater;
use crate::engine::log::Session;

pub enum LoadError {
    Missing(String),
    V1(String),
    Corrupt(String),
}

impl LoadError {
    pub fn message(&self) -> &str {
        match self {
            LoadError::Missing(m) | LoadError::V1(m) | LoadError::Corrupt(m) => m,
        }
    }
}

pub struct Store {
    root: PathBuf,
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .map_err(|e| format!("chmod {}: {e}", path.display()))
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> Result<(), String> {
    Ok(())
}

fn private_file(opts: &mut OpenOptions) -> &mut OpenOptions {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts
}

impl Store {
    pub fn from_env() -> Store {
        let root = match std::env::var("XDG_CONFIG_HOME") {
            Ok(x) if !x.is_empty() => PathBuf::from(x).join("0sum"),
            _ => PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into())).join(".0sum"),
        };
        Store { root }
    }

    #[cfg(test)]
    pub fn at(root: PathBuf) -> Store {
        Store { root }
    }

    fn sessions(&self) -> PathBuf {
        self.root.join("sessions")
    }

    fn session_path(&self, sid: &str) -> PathBuf {
        self.sessions().join(format!("{sid}.json"))
    }

    /// Create the dirs 0700 and re-assert 0700 on every run
    /// (`DirBuilder::mode` only applies at creation — the #6 trap).
    pub fn prepare(&self) -> Result<(), String> {
        let mut b = fs::DirBuilder::new();
        b.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            b.mode(0o700);
        }
        b.create(self.sessions())
            .map_err(|e| format!("create {}: {e}", self.sessions().display()))?;
        set_mode(&self.root, 0o700)?;
        set_mode(&self.sessions(), 0o700)
    }

    /// v2 never reads, writes, deletes or chmods the v1 index; it only says so.
    pub fn legacy_index_warning(&self) -> Option<String> {
        let path = self.root.join("index.json");
        path.exists().then(|| {
            format!(
                "warning: legacy v1 index.json at {} holds plaintext passwords; v2 doesn't use it — delete it yourself when you're done with v1 games",
                path.display()
            )
        })
    }

    /// Exclusive advisory lock on `<SID>.lock`, held for the returned File's
    /// lifetime. Not the JSON: the atomic rename swaps its inode.
    pub fn lock(&self, sid: &str) -> Result<File, String> {
        if !self.session_path(sid).exists() {
            return Err(format!("no session `{sid}` found"));
        }
        let path = self.sessions().join(format!("{sid}.lock"));
        let f = private_file(OpenOptions::new().read(true).write(true).create(true))
            .open(&path)
            .map_err(|e| format!("open {}: {e}", path.display()))?;
        set_mode(&path, 0o600)?;
        f.lock()
            .map_err(|e| format!("lock {}: {e}", path.display()))?;
        Ok(f)
    }

    /// Claim a fresh SID. `Ok(false)` if that SID is taken.
    pub fn create(&self, s: &Session) -> Result<bool, String> {
        let path = self.session_path(&s.session_id);
        let mut f = match private_file(OpenOptions::new().write(true).create_new(true)).open(&path)
        {
            Ok(f) => f,
            Err(e) if e.kind() == ErrorKind::AlreadyExists => return Ok(false),
            Err(e) => return Err(format!("create {}: {e}", path.display())),
        };
        f.write_all(json(s)?.as_bytes())
            .and_then(|_| f.sync_all())
            .map_err(|e| format!("write {}: {e}", path.display()))?;
        Ok(true)
    }

    /// Atomic: write `<SID>.json.tmp` (0600), then rename over `<SID>.json`.
    pub fn save(&self, s: &Session) -> Result<(), String> {
        atomic_write(&self.session_path(&s.session_id), &json(s)?)
    }

    fn hw_path(&self, sid: &str, seat: &str) -> PathBuf {
        self.sessions().join(format!("{sid}.{seat}.hw"))
    }

    /// The seat's high-water mark file (A4), if there is one.
    pub fn read_hw(&self, sid: &str, seat: &str) -> Result<Option<String>, String> {
        let path = self.hw_path(sid, seat);
        match fs::read_to_string(&path) {
            Ok(t) => Ok(Some(t)),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("read {}: {e}", path.display())),
        }
    }

    pub fn write_hw(&self, sid: &str, seat: &str, hw: &HighWater) -> Result<(), String> {
        let text = serde_json::to_string(hw).map_err(|e| format!("serialize mark: {e}"))?;
        atomic_write(&self.hw_path(sid, seat), &text)
    }

    pub fn load(&self, sid: &str) -> Result<Session, LoadError> {
        let path = self.session_path(sid);
        let text = fs::read_to_string(&path)
            .map_err(|_| LoadError::Missing(format!("no session `{sid}` found")))?;
        let corrupt =
            |e: String| LoadError::Corrupt(format!("session file doesn't parse: {}", escape(&e)));
        let raw: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| corrupt(e.to_string()))?;
        if raw.get("version") == Some(&serde_json::json!(1)) {
            return Err(LoadError::V1(format!(
                "session {sid} is a v1 (unsigned) session and can't be played by this version — start a new one"
            )));
        }
        let s: Session = serde_json::from_value(raw).map_err(|e| corrupt(e.to_string()))?;
        if s.session_id != sid {
            return Err(LoadError::Corrupt(format!(
                "{sid}.json claims to be session {}",
                escape(&s.session_id)
            )));
        }
        set_mode(&path, 0o600).map_err(LoadError::Corrupt)?;
        Ok(s)
    }
}

fn atomic_write(path: &Path, text: &str) -> Result<(), String> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let mut f = private_file(OpenOptions::new().write(true).create(true).truncate(true))
        .open(&tmp)
        .map_err(|e| format!("open {}: {e}", tmp.display()))?;
    set_mode(&tmp, 0o600)?;
    f.write_all(text.as_bytes())
        .and_then(|_| f.sync_all())
        .map_err(|e| format!("write {}: {e}", tmp.display()))?;
    fs::rename(&tmp, path).map_err(|e| format!("rename into {}: {e}", path.display()))
}

fn json(s: &Session) -> Result<String, String> {
    serde_json::to_string_pretty(s).map_err(|e| format!("serialize session: {e}"))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::engine::new_session;
    use crate::farkle::ruleset::RuleSet;
    use crate::farkle::Farkle;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicU32, Ordering};

    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new() -> TempRoot {
            static N: AtomicU32 = AtomicU32::new(0);
            let p = std::env::temp_dir().join(format!(
                "0sum-storage-test-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = fs::remove_dir_all(&p);
            TempRoot(p)
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn session() -> Session {
        let names = vec!["Alice".to_string(), "Bob".to_string()];
        new_session::<Farkle>("TEST01", &RuleSet::kcd(), &names, [0; 16]).unwrap()
    }

    fn mode(p: &Path) -> u32 {
        fs::metadata(p).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn writes_leave_private_modes_even_over_loose_ones() {
        let tmp = TempRoot::new();
        let store = Store::at(tmp.0.clone());
        fs::create_dir_all(tmp.0.join("sessions")).unwrap();
        set_mode(&tmp.0, 0o755).unwrap();
        set_mode(&tmp.0.join("sessions"), 0o755).unwrap();
        fs::write(tmp.0.join("sessions/TEST01.json.tmp"), "stale").unwrap();
        set_mode(&tmp.0.join("sessions/TEST01.json.tmp"), 0o644).unwrap();

        store.prepare().unwrap();
        store.save(&session()).unwrap();
        drop(store.lock("TEST01").unwrap());

        assert_eq!(mode(&tmp.0), 0o700);
        assert_eq!(mode(&tmp.0.join("sessions")), 0o700);
        assert_eq!(mode(&tmp.0.join("sessions/TEST01.json")), 0o600);
        assert_eq!(mode(&tmp.0.join("sessions/TEST01.lock")), 0o600);
        assert!(!tmp.0.join("sessions/TEST01.json.tmp").exists());
        assert_eq!(store.load("TEST01").ok().unwrap().log.len(), 1);
    }

    #[test]
    fn create_refuses_a_taken_sid() {
        let tmp = TempRoot::new();
        let store = Store::at(tmp.0.clone());
        store.prepare().unwrap();
        assert!(store.create(&session()).unwrap());
        assert!(!store.create(&session()).unwrap());
        assert_eq!(mode(&tmp.0.join("sessions/TEST01.json")), 0o600);
    }

    #[test]
    fn legacy_index_is_warned_about_and_left_alone() {
        let tmp = TempRoot::new();
        let store = Store::at(tmp.0.clone());
        store.prepare().unwrap();
        assert!(store.legacy_index_warning().is_none());
        let index = tmp.0.join("index.json");
        fs::write(&index, r#"{"PW":{"session_id":"OLD001","player_id":"p1"}}"#).unwrap();
        set_mode(&index, 0o644).unwrap();

        let w = store.legacy_index_warning().unwrap();
        assert_eq!(
            w,
            format!("warning: legacy v1 index.json at {} holds plaintext passwords; v2 doesn't use it — delete it yourself when you're done with v1 games", index.display())
        );
        store.prepare().unwrap();
        store.save(&session()).unwrap();
        assert!(index.exists());
        assert_eq!(mode(&index), 0o644);
    }

    #[test]
    fn v1_sessions_are_refused() {
        let tmp = TempRoot::new();
        let store = Store::at(tmp.0.clone());
        store.prepare().unwrap();
        fs::write(
            tmp.0.join("sessions/OLD001.json"),
            r#"{"version":1,"session_id":"OLD001","log":[]}"#,
        )
        .unwrap();
        match store.load("OLD001") {
            Err(LoadError::V1(m)) => assert_eq!(m, "session OLD001 is a v1 (unsigned) session and can't be played by this version — start a new one"),
            _ => panic!("v1 must be refused"),
        }
    }

    #[test]
    fn a_file_must_match_its_name() {
        let tmp = TempRoot::new();
        let store = Store::at(tmp.0.clone());
        store.prepare().unwrap();
        store.save(&session()).unwrap();
        fs::rename(
            tmp.0.join("sessions/TEST01.json"),
            tmp.0.join("sessions/TEST02.json"),
        )
        .unwrap();
        assert!(matches!(store.load("TEST02"), Err(LoadError::Corrupt(_))));
    }

    #[test]
    fn the_lock_is_exclusive() {
        let tmp = TempRoot::new();
        let store = Store::at(tmp.0.clone());
        store.prepare().unwrap();
        store.save(&session()).unwrap();
        let held = store.lock("TEST01").unwrap();
        let other = File::open(tmp.0.join("sessions/TEST01.lock")).unwrap();
        assert!(other.try_lock().is_err());
        drop(held);
        assert!(other.try_lock().is_ok());
    }
}
