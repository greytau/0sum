//! Local persistence (PHASE2 §6): `$XDG_CONFIG_HOME/0sum` or `~/.0sum`, one
//! JSON file per session plus an advisory lock file. Modes are hygiene against
//! other unix users only; players sharing a uid read everything, which is why
//! nothing secret is ever stored.

use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use rand::rngs::OsRng;
use rand::RngCore;

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

    /// Atomic: write a fresh `<SID>.json.<rand>.tmp` (0600), then rename over `<SID>.json`.
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
        let text = match fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == ErrorKind::NotFound => {
                return Err(LoadError::Missing(format!("no session `{sid}` found")))
            }
            Err(e) => {
                return Err(LoadError::Corrupt(format!(
                    "can't read {sid}.json: {}",
                    escape(&e.to_string())
                )))
            }
        };
        let corrupt =
            |e: String| LoadError::Corrupt(format!("session file doesn't parse: {}", escape(&e)));
        let raw: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| corrupt(e.to_string()))?;
        if raw.get("version") == Some(&serde_json::json!(1)) {
            return Err(LoadError::V1(format!(
                "session {sid} is a v1 (unsigned) session and can't be played by this version — start a new one"
            )));
        }
        // From the text, not the Value: a Value keeps only the last of two
        // duplicate keys, so a decoy `author` would vanish unnoticed (H4).
        let s: Session = serde_json::from_str(&text).map_err(|e| corrupt(e.to_string()))?;
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

/// `<path>.<16 random hex>.tmp`, created fresh: a file or directory planted at
/// a predictable temp name can't make the write fail (H1).
fn atomic_write(path: &Path, text: &str) -> Result<(), String> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".{:016x}.tmp", OsRng.next_u64()));
    let tmp = PathBuf::from(tmp);
    let mut f = private_file(OpenOptions::new().write(true).create_new(true))
        .open(&tmp)
        .map_err(|e| format!("open {}: {e}", tmp.display()))?;
    let written = set_mode(&tmp, 0o600)
        .and_then(|()| {
            f.write_all(text.as_bytes())
                .and_then(|_| f.sync_all())
                .map_err(|e| format!("write {}: {e}", tmp.display()))
        })
        .and_then(|()| {
            fs::rename(&tmp, path).map_err(|e| format!("rename into {}: {e}", path.display()))
        });
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written
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

        store.prepare().unwrap();
        store.save(&session()).unwrap();
        drop(store.lock("TEST01").unwrap());

        assert_eq!(mode(&tmp.0), 0o700);
        assert_eq!(mode(&tmp.0.join("sessions")), 0o700);
        assert_eq!(mode(&tmp.0.join("sessions/TEST01.json")), 0o600);
        assert_eq!(mode(&tmp.0.join("sessions/TEST01.lock")), 0o600);
        assert_eq!(temp_files(&tmp.0), Vec::<String>::new());
        assert_eq!(store.load("TEST01").ok().unwrap().log.len(), 1);
    }

    fn temp_files(root: &Path) -> Vec<String> {
        fs::read_dir(root.join("sessions"))
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .filter(|n| n.ends_with(".tmp"))
            .collect()
    }

    /// H1: whatever squats on the old fixed temp name, or on any other temp
    /// name, the write goes through and leaves the squatter alone.
    #[test]
    fn a_planted_temp_name_cant_stop_a_write() {
        use crate::engine::hw;
        use crate::engine::testkit::seated;

        let tmp = TempRoot::new();
        let store = Store::at(tmp.0.clone());
        store.prepare().unwrap();
        let (s, _, players) = seated::<Farkle>(&RuleSet::kcd(), &["Alice", "Bob"]);
        let dir = tmp.0.join("sessions");
        let squatters = [
            "TEST01.json.tmp",
            "TEST01.p1.hw.tmp",
            "TEST01.json.0000000000000000.tmp",
            "TEST01.p1.hw.ffffffffffffffff.tmp",
        ];
        for name in squatters {
            fs::create_dir(dir.join(name)).unwrap();
        }
        fs::write(dir.join("TEST01.json.tmp/x"), "stale").unwrap();

        store.save(&s).unwrap();
        store
            .write_hw("TEST01", "p1", &hw::mark(&s, &players[0].sk))
            .unwrap();
        assert_eq!(store.load("TEST01").ok().unwrap().log.len(), s.log.len());
        assert!(store.read_hw("TEST01", "p1").unwrap().is_some());
        let mut left = temp_files(&tmp.0);
        left.sort();
        let mut want: Vec<String> = squatters.iter().map(|n| n.to_string()).collect();
        want.sort();
        assert_eq!(left, want);
        assert_eq!(
            fs::read_to_string(dir.join("TEST01.json.tmp/x")).unwrap(),
            "stale"
        );
    }

    /// A failed write takes its temp file with it.
    #[test]
    fn a_failed_write_leaves_no_temp_file() {
        let tmp = TempRoot::new();
        let store = Store::at(tmp.0.clone());
        store.prepare().unwrap();
        fs::create_dir(tmp.0.join("sessions/TEST01.json")).unwrap();
        fs::write(tmp.0.join("sessions/TEST01.json/x"), "").unwrap();
        assert!(store
            .save(&session())
            .unwrap_err()
            .starts_with("rename into "));
        assert_eq!(temp_files(&tmp.0), Vec::<String>::new());
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

    /// A signed Liar's Dice log survives save → load byte for byte, and the
    /// loaded copy replays to the same state and head.
    #[test]
    fn a_session_round_trips_through_save_and_load() {
        use crate::engine::testkit::seated;
        use crate::engine::{apply_chain, replay};
        use crate::liars_dice::{ruleset::RuleSet as Ld, LiarsDice, Move};

        let tmp = TempRoot::new();
        let store = Store::at(tmp.0.clone());
        store.prepare().unwrap();
        let (mut s, mut t, mut players) = seated::<LiarsDice>(&Ld::classic(), &["Alice", "Bob"]);
        assert!(store.create(&s).unwrap());
        for (seat, moves) in [(0, vec![]), (1, vec![]), (0, vec![Move::Bid(2, 6)])] {
            let p = &mut players[seat];
            assert_eq!(
                apply_chain(&mut s, &mut t, seat, &moves, &mut p.ctx, &p.sk).failure,
                None
            );
            store.save(&s).unwrap();
        }
        let loaded = store.load("TEST01").ok().unwrap();
        assert_eq!(
            serde_json::to_string(&loaded).unwrap(),
            serde_json::to_string(&s).unwrap()
        );
        let again = replay::<LiarsDice>(&loaded).unwrap();
        assert_eq!((again.state, again.head), (t.state, t.head));
        store.save(&loaded).unwrap();
        assert_eq!(store.load("TEST01").ok().unwrap().log.len(), s.log.len());
    }

    #[test]
    fn marks_are_private_atomic_and_read_back() {
        use crate::engine::hw;
        use crate::engine::testkit::seated;

        let tmp = TempRoot::new();
        let store = Store::at(tmp.0.clone());
        store.prepare().unwrap();
        let (s, _, players) = seated::<Farkle>(&RuleSet::kcd(), &["Alice", "Bob"]);
        store.save(&s).unwrap();
        assert_eq!(store.read_hw("TEST01", "p1").unwrap(), None);
        let path = tmp.0.join("sessions/TEST01.p1.hw");
        fs::write(&path, "old").unwrap();
        set_mode(&path, 0o644).unwrap();

        store
            .write_hw("TEST01", "p1", &hw::mark(&s, &players[0].sk))
            .unwrap();
        assert_eq!(mode(&path), 0o600);
        assert_eq!(temp_files(&tmp.0), Vec::<String>::new());
        let raw = store.read_hw("TEST01", "p1").unwrap().unwrap();
        hw::check(&s, "p1", &players[0].sk.verifying_key(), Some(&raw)).unwrap();
        assert_eq!(store.read_hw("TEST01", "p2").unwrap(), None);
    }

    #[test]
    fn locking_needs_an_existing_session() {
        let tmp = TempRoot::new();
        let store = Store::at(tmp.0.clone());
        store.prepare().unwrap();
        assert_eq!(
            store.lock("TEST01").unwrap_err(),
            "no session `TEST01` found"
        );
        assert!(!tmp.0.join("sessions/TEST01.lock").exists());
    }

    /// A6: serde's error text quotes the file back; it reaches the terminal inert.
    #[test]
    fn a_corrupt_file_is_reported_inertly() {
        let tmp = TempRoot::new();
        let store = Store::at(tmp.0.clone());
        store.prepare().unwrap();
        let mut v = serde_json::to_value(session()).unwrap();
        v["\u{1b}]0;pwned\u{7}"] = serde_json::json!(1);
        fs::write(tmp.0.join("sessions/TEST01.json"), v.to_string()).unwrap();
        let Err(LoadError::Corrupt(m)) = store.load("TEST01") else {
            panic!("an unknown key must be refused")
        };
        assert!(
            m.starts_with("session file doesn't parse: unknown field `\\u{1b}]0;pwned\\u{7}`"),
            "{m}"
        );
        assert!(!m.contains(['\u{1b}', '\u{7}']));
    }

    /// H4: a second `author` key in an entry is refused, not collapsed.
    #[test]
    fn a_duplicate_key_in_an_entry_is_refused() {
        use crate::engine::testkit::seated;

        let tmp = TempRoot::new();
        let store = Store::at(tmp.0.clone());
        store.prepare().unwrap();
        let (s, _, _) = seated::<Farkle>(&RuleSet::kcd(), &["Alice", "Bob"]);
        store.save(&s).unwrap();
        let path = tmp.0.join("sessions/TEST01.json");
        let text = fs::read_to_string(&path).unwrap();
        let doctored = text.replacen(
            r#""author": "p2","#,
            r#""author": "p1", "author": "p2","#,
            1,
        );
        assert_ne!(doctored, text);
        fs::write(&path, doctored).unwrap();
        let Err(LoadError::Corrupt(m)) = store.load("TEST01") else {
            panic!("a duplicate key must be refused")
        };
        assert!(
            m.starts_with("session file doesn't parse: duplicate field `author`"),
            "{m}"
        );
    }

    /// D2: only a missing file is "no session"; anything else says what broke.
    #[test]
    fn an_unreadable_session_isnt_reported_missing() {
        let tmp = TempRoot::new();
        let store = Store::at(tmp.0.clone());
        store.prepare().unwrap();
        assert!(matches!(store.load("TEST01"), Err(LoadError::Missing(_))));
        fs::create_dir(tmp.0.join("sessions/TEST01.json")).unwrap();
        let Err(LoadError::Corrupt(m)) = store.load("TEST01") else {
            panic!("a directory isn't a missing session")
        };
        assert!(m.starts_with("can't read TEST01.json: "), "{m}");
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
