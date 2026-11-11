//! End-to-end through the real binary, in a throwaway XDG_CONFIG_HOME/HOME.

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

struct Sandbox(PathBuf);

impl Sandbox {
    fn new() -> Sandbox {
        static N: AtomicU32 = AtomicU32::new(0);
        let p = std::env::temp_dir().join(format!(
            "0sum-cli-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        Sandbox(p)
    }

    fn root(&self) -> PathBuf {
        self.0.join("0sum")
    }

    fn run(&self, args: &[&str], stdin: &str, password: Option<&str>) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_0sum"));
        cmd.args(args)
            .env("XDG_CONFIG_HOME", &self.0)
            .env("HOME", &self.0)
            .env_remove("ZEROSUM_PASSWORD")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(pw) = password {
            cmd.env("ZEROSUM_PASSWORD", pw);
        }
        let mut child = cmd.spawn().unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    }

    fn sessions(&self) -> Vec<String> {
        fs::read_dir(self.root().join("sessions"))
            .map(|d| {
                d.map(|e| e.unwrap().file_name().into_string().unwrap())
                    .filter(|n| n.ends_with(".json"))
                    .collect()
            })
            .unwrap_or_default()
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn after<'a>(text: &'a str, marker: &str) -> &'a str {
    let line = text.lines().find(|l| l.contains(marker)).unwrap();
    line.split(marker).nth(1).unwrap().trim()
}

#[test]
fn new_rejects_names_that_differ_only_in_case() {
    let sb = Sandbox::new();
    let o = sb.run(&["new", "farkle"], "2\nAlice\nalice\n", None);
    assert!(!o.status.success());
    assert!(
        err(&o).contains("names must differ ignoring case"),
        "{}",
        err(&o)
    );
    assert!(sb.sessions().is_empty());
}

#[test]
fn legacy_index_survives_and_is_warned_about() {
    let sb = Sandbox::new();
    fs::create_dir_all(sb.root()).unwrap();
    let index = sb.root().join("index.json");
    fs::write(&index, "{}").unwrap();
    let o = sb.run(&["new", "farkle"], "2\nAlice\nBob\n", None);
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(
        err(&o).trim(),
        format!("warning: legacy v1 index.json at {} holds plaintext passwords; v2 doesn't use it — delete it yourself when you're done with v1 games", index.display())
    );
    assert_eq!(fs::read_to_string(&index).unwrap(), "{}");
}

#[test]
fn v1_session_is_refused() {
    let sb = Sandbox::new();
    fs::create_dir_all(sb.root().join("sessions")).unwrap();
    fs::write(
        sb.root().join("sessions/AB12CD.json"),
        r#"{"version":1,"session_id":"AB12CD","log":[]}"#,
    )
    .unwrap();
    let o = sb.run(&["register", "AB12CD", "Alice"], "", None);
    assert!(err(&o).contains("is a v1 (unsigned) session and can't be played by this version"));
}

#[test]
fn a_two_player_session_end_to_end() {
    let sb = Sandbox::new();
    let o = sb.run(&["new", "farkle"], "2\nAlice\nBob\n", None);
    let sid = after(&out(&o), "SESSION ID:").to_string();

    let o = sb.run(&["register", &sid, "alice"], "", None);
    assert!(o.status.success(), "{}", err(&o));
    let alice = after(&out(&o), "PASSWORD (keep secret):").to_string();
    assert!(out(&o).contains("Waiting on: Bob"));
    let o = sb.run(&["register", &sid, "Bob"], "", None);
    let bob = after(&out(&o), "PASSWORD (keep secret):").to_string();
    assert!(out(&o).contains("Alice's turn — roll: zs play roll"));

    let o = sb.run(&["play", "roll"], "", None);
    assert!(err(&o).contains("set ZEROSUM_PASSWORD"));
    let o = sb.run(&["play", &alice, "roll"], "", Some(&alice));
    assert!(err(&o).contains("don't put your password on the command line"));
    let o = sb.run(&["play", "roll"], "", Some(&bob));
    assert!(err(&o).contains("it's Alice's turn, not yours"));

    let o = sb.run(&["play"], "", Some(&bob));
    assert!(o.status.success());
    assert!(out(&o).contains("--- scores (target 4000) ---"));
    let o = sb.run(&["play", "roll"], "", Some(&alice));
    assert!(o.status.success(), "{}", err(&o));
    assert!(out(&o).contains("Alice rolled: "));

    let o = sb.run(&["verify", &sid], "", None);
    assert!(o.status.success(), "{}", out(&o));
    assert!(out(&o).contains("OK  4 entries  head 3:"));

    let path = sb.root().join(format!("sessions/{sid}.json"));
    let text = fs::read_to_string(&path).unwrap();
    assert!(!text.contains(&alice[7..]) && !text.contains(&bob[7..]));
    fs::write(
        &path,
        text.replacen("\"rolled\"", "\"rolled\" ", 1)
            .replacen("\"ts\": ", "\"ts\": 1", 1),
    )
    .unwrap();
    let o = sb.run(&["verify", &sid], "", None);
    assert!(!o.status.success());
    assert!(
        out(&o).contains("BROKEN at seq 0: hash doesn't match"),
        "{}",
        out(&o)
    );
}

fn liars_dice_table(sb: &Sandbox) -> (String, String, String) {
    let o = sb.run(&["new", "ld"], "2\nAlice\nBob\n", None);
    assert!(o.status.success(), "{}", err(&o));
    let sid = after(&out(&o), "SESSION ID:").to_string();
    let o = sb.run(&["register", &sid, "Alice"], "", None);
    assert!(
        out(&o).contains(r#"read -rs ZP; zs() { ZEROSUM_PASSWORD="$ZP" 0sum "$@"; }"#),
        "{}",
        out(&o)
    );
    assert!(!out(&o).contains("export"));
    let alice = after(&out(&o), "PASSWORD (keep secret):").to_string();
    let o = sb.run(&["register", &sid, "Bob"], "", None);
    assert!(out(&o).contains("Bob committed to round 1 dice"));
    let bob = after(&out(&o), "PASSWORD (keep secret):").to_string();
    (sid, alice, bob)
}

#[test]
fn liars_dice_two_terminals_to_a_winner() {
    let sb = Sandbox::new();
    let (sid, alice, bob) = liars_dice_table(&sb);
    let pws = [&alice, &bob];
    let mut winner = None;
    'game: for _ in 0..100 {
        for pw in pws {
            let o = sb.run(&["play"], "", Some(pw));
            assert!(o.status.success(), "{}", err(&o));
            let text = out(&o);
            assert!(text.contains("\nhead "), "{text}");
            assert!(text.contains("your last move: "), "{text}");
            if let Some(line) = text.lines().find(|l| l.contains("WINS!")) {
                winner = Some(line.to_string());
                break 'game;
            }
            if text.contains("Your move") {
                let mv: &[&str] = if text.contains("(beat ") {
                    &["play", "liar"]
                } else {
                    &["play", "bid", "1", "2"]
                };
                let o = sb.run(mv, "", Some(pw));
                assert!(o.status.success(), "{}{}", out(&o), err(&o));
            }
        }
    }
    let winner = winner.expect("the game ends");
    assert!(winner.contains("Alice") || winner.contains("Bob"));
    let o = sb.run(&["verify", &sid], "", None);
    assert!(o.status.success(), "{}", out(&o));
    assert!(out(&o).contains("wins the game!"));
    assert!(out(&o).lines().last().unwrap().starts_with("OK  "));
}

/// A4 against §12.2's harvest attack: Bob snapshots, calls, Alice's duty
/// reveals her dice, Bob restores the snapshot. Alice's next run refuses
/// and writes nothing.
#[test]
fn a_rolled_back_log_is_refused_with_rollback() {
    let sb = Sandbox::new();
    let (sid, alice, bob) = liars_dice_table(&sb);
    sb.run(&["play"], "", Some(&alice)); // 4 commit, 5 entropy
    sb.run(&["play"], "", Some(&bob)); // 6 entropy
    let o = sb.run(&["play", "bid", "1", "2"], "", Some(&alice)); // 7
    assert!(o.status.success(), "{}", err(&o));

    let path = sb.root().join(format!("sessions/{sid}.json"));
    let snapshot = fs::read_to_string(&path).unwrap();
    let o = sb.run(&["play", "liar"], "", Some(&bob)); // 8 call, 9 reveal, 10 commit
    assert!(out(&o).contains("waiting on Alice's reveal"), "{}", out(&o));
    let o = sb.run(&["play"], "", Some(&alice)); // 11 reveal → resolve, 12, 13
    assert!(out(&o).contains("Round 1 result: Bob called liar on Alice's 1×2."));
    assert!(out(&o).contains("head 13:"));

    fs::write(&path, &snapshot).unwrap();
    let mark = sb.root().join(format!("sessions/{sid}.p1.hw"));
    let mark_before = fs::read(&mark).unwrap();
    let o = sb.run(&["play"], "", Some(&alice));
    assert_eq!(o.status.code(), Some(1));
    assert!(
        err(&o).contains("ROLLBACK: you last saw seq 13:")
            && err(&o).contains("this log no longer contains it. Don't play on. Run 0sum verify."),
        "{}",
        err(&o)
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), snapshot);
    assert_eq!(fs::read(&mark).unwrap(), mark_before);

    // The restored log is itself valid: only the mark can tell.
    let o = sb.run(&["verify", &sid], "", None);
    assert!(o.status.success());
}

#[test]
fn new_rejects_names_that_could_carry_escapes() {
    let sb = Sandbox::new();
    let o = sb.run(&["new", "ld"], "2\nAlice\nBob\x1b[2J\n", None);
    assert!(!o.status.success());
    assert!(err(&o).contains(r"Bob\u{1b}[2J"), "{}", err(&o));
    assert!(!err(&o).contains('\x1b'));
    assert!(sb.sessions().is_empty());
}
