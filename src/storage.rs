//! Local persistence: session files and the password index, both under the
//! 0sum home directory (`$XDG_CONFIG_HOME/0sum` or `~/.0sum`).
//!
//! The index maps a per-player password to its session + seat, so `play
//! <password> <move>` needs no session id re-type. Phase-1 passwords are opaque
//! random tokens; the crypto-key upgrade is a later chunk.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use serde::{Deserialize, Serialize};

use crate::session::Session;

/// Crockford base32 — unambiguous, hand-writable (no I/L/O/U).
const ALPHABET: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexEntry {
    pub session_id: String,
    pub player_id: String,
}

fn home() -> PathBuf {
    if let Ok(x) = std::env::var("XDG_CONFIG_HOME") {
        if !x.is_empty() {
            return PathBuf::from(x).join("0sum");
        }
    }
    let h = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(h).join(".0sum")
}

fn sessions_dir() -> PathBuf {
    home().join("sessions")
}

fn index_path() -> PathBuf {
    home().join("index.json")
}

fn session_path(session_id: &str) -> PathBuf {
    sessions_dir().join(format!("{session_id}.json"))
}

fn rng() -> StdRng {
    StdRng::from_entropy()
}

fn random_token(len: usize) -> String {
    let mut r = rng();
    (0..len)
        .map(|_| ALPHABET[r.gen_range(0..ALPHABET.len())] as char)
        .collect()
}

/// Short, human-writable public room code.
pub fn gen_session_id() -> String {
    random_token(6)
}

/// Longer opaque per-player secret (a real signing key in a later chunk).
pub fn gen_password() -> String {
    random_token(26)
}

pub fn save_session(session: &Session) -> Result<(), String> {
    fs::create_dir_all(sessions_dir()).map_err(|e| format!("create sessions dir: {e}"))?;
    let json = serde_json::to_string_pretty(session).map_err(|e| format!("serialize: {e}"))?;
    fs::write(session_path(&session.session_id), json).map_err(|e| format!("write session: {e}"))
}

pub fn load_session(session_id: &str) -> Result<Session, String> {
    let path = session_path(session_id);
    let json = fs::read_to_string(&path).map_err(|_| format!("no session `{session_id}` found"))?;
    serde_json::from_str(&json).map_err(|e| format!("corrupt session file: {e}"))
}

fn load_index() -> BTreeMap<String, IndexEntry> {
    fs::read_to_string(index_path())
        .ok()
        .and_then(|j| serde_json::from_str(&j).ok())
        .unwrap_or_default()
}

fn save_index(index: &BTreeMap<String, IndexEntry>) -> Result<(), String> {
    fs::create_dir_all(home()).map_err(|e| format!("create home dir: {e}"))?;
    let json = serde_json::to_string_pretty(index).map_err(|e| format!("serialize index: {e}"))?;
    fs::write(index_path(), json).map_err(|e| format!("write index: {e}"))
}

pub fn lookup_password(password: &str) -> Option<IndexEntry> {
    load_index().get(password).cloned()
}

pub fn add_password(password: &str, entry: IndexEntry) -> Result<(), String> {
    let mut index = load_index();
    index.insert(password.to_string(), entry);
    save_index(&index)
}
