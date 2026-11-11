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
    for args in [&["play", &alice, "roll"][..], &["play", "roll", &alice]] {
        let o = sb.run(args, "", Some(&alice));
        assert!(err(&o).contains("don't put your password on the command line"));
    }
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
    assert_eq!(out(&o).matches("Round 1 result:").count(), 1, "{}", out(&o));
    assert!(out(&o).contains("head 13:"));
    let o = sb.run(&["play"], "", Some(&bob));
    assert_eq!(out(&o).matches("Round 1 result:").count(), 1, "{}", out(&o));

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

#[cfg(unix)]
fn assert_private_and_clean(sb: &Sandbox) {
    use std::os::unix::fs::PermissionsExt;
    let mode = |p: &PathBuf| fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&sb.root()), 0o700);
    assert_eq!(mode(&sb.root().join("sessions")), 0o700);
    for e in fs::read_dir(sb.root().join("sessions")).unwrap() {
        let p = e.unwrap().path();
        assert_eq!(mode(&p), 0o600, "{}", p.display());
        assert!(!p.to_string_lossy().ends_with(".tmp"), "{}", p.display());
    }
}

/// A4, the cheater's own view too: Alice snapshots before her bid, learns
/// Bob's dice from the reveals, and restores. Both seats' binaries refuse the
/// restored log, with or without a move, and write nothing.
#[test]
fn a_rollback_after_reveal_stops_both_seats() {
    let sb = Sandbox::new();
    let (sid, alice, bob) = liars_dice_table(&sb);
    sb.run(&["play"], "", Some(&alice)); // 4 commit, 5 entropy
    sb.run(&["play"], "", Some(&bob)); // 6 entropy
    let path = sb.root().join(format!("sessions/{sid}.json"));
    let snapshot = fs::read_to_string(&path).unwrap();
    sb.run(&["play", "bid", "1", "2"], "", Some(&alice)); // 7
    sb.run(&["play", "liar"], "", Some(&bob)); // 8 call, 9 reveal, 10 commit
    let o = sb.run(&["play"], "", Some(&alice)); // 11 reveal → resolve, 12, 13
    assert!(out(&o).contains("Round 1 result: Bob called liar on Alice's 1×2."));
    #[cfg(unix)]
    assert_private_and_clean(&sb);

    fs::write(&path, &snapshot).unwrap();
    let marks = |who: &str| fs::read(sb.root().join(format!("sessions/{sid}.{who}.hw"))).unwrap();
    let before = (marks("p1"), marks("p2"));
    for (pw, seen) in [(&alice, 13), (&bob, 10)] {
        for args in [&["play"][..], &["play", "bid", "2", "2"]] {
            let o = sb.run(args, "", Some(pw));
            assert_eq!(o.status.code(), Some(1));
            assert!(
                err(&o).contains(&format!("ROLLBACK: you last saw seq {seen}:")),
                "{}",
                err(&o)
            );
            assert_eq!(out(&o), "");
        }
    }
    assert_eq!(fs::read_to_string(&path).unwrap(), snapshot);
    assert_eq!((marks("p1"), marks("p2")), before);
}

/// Bob's key doesn't exist until he registers, so no mark for his seat can
/// be his. A planted one stops registration loudly: no seat, no password.
#[test]
fn a_mark_planted_for_an_unregistered_seat_refuses_registration() {
    let sb = Sandbox::new();
    let o = sb.run(&["new", "ld"], "2\nAlice\nBob\n", None);
    let sid = after(&out(&o), "SESSION ID:").to_string();
    sb.run(&["register", &sid, "Alice"], "", None);
    let dir = sb.root().join("sessions");
    let path = dir.join(format!("{sid}.json"));
    let log = fs::read_to_string(&path).unwrap();
    let planted = dir.join(format!("{sid}.p2.hw"));
    for mark in [
        fs::read_to_string(dir.join(format!("{sid}.p1.hw"))).unwrap(),
        "{}".into(),
    ] {
        fs::write(&planted, &mark).unwrap();
        let o = sb.run(&["register", &sid, "Bob"], "", None);
        assert_eq!(o.status.code(), Some(1));
        assert_eq!(
            err(&o).trim(),
            "error: seat p2 has a high-water mark but no registration in this log: the log was rolled back past Bob's register, or the file was planted. Run 0sum verify."
        );
        assert!(!out(&o).contains("PASSWORD"));
        assert_eq!(fs::read_to_string(&path).unwrap(), log);
        assert_eq!(fs::read_to_string(&planted).unwrap(), mark);
    }
    fs::remove_file(&planted).unwrap();
    let o = sb.run(&["register", &sid, "Bob"], "", None);
    assert!(o.status.success(), "{}", err(&o));
}

#[test]
fn registering_a_taken_seat_again_is_refused() {
    let sb = Sandbox::new();
    let (sid, _, bob) = liars_dice_table(&sb);
    let path = sb.root().join(format!("sessions/{sid}.json"));
    let log = fs::read_to_string(&path).unwrap();
    let o = sb.run(&["register", &sid, "BOB"], "", None);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(err(&o).trim(), "error: `Bob` has already registered");
    assert!(!out(&o).contains("PASSWORD"));
    assert_eq!(fs::read_to_string(&path).unwrap(), log);
    assert!(sb.run(&["play"], "", Some(&bob)).status.success());
}

/// A6 end to end: an unknown key smuggling escapes into the session file or
/// a mark comes back out of `verify` and `play` as inert text.
#[test]
fn escapes_in_file_keys_reach_the_terminal_inert() {
    let sb = Sandbox::new();
    let (sid, alice, _) = liars_dice_table(&sb);
    let path = sb.root().join(format!("sessions/{sid}.json"));
    let good = fs::read_to_string(&path).unwrap();
    let raw = |o: &Output| [&o.stdout[..], &o.stderr[..]].concat();
    let evil = r#""\u001b]0;pwned\u0007\u202e": 1,"#;
    fs::write(&path, good.replacen('{', &format!("{{{evil}"), 1)).unwrap();
    for (args, pw) in [(&["verify", &sid][..], None), (&["play"][..], Some(&alice))] {
        let o = sb.run(args, "", pw.map(|p| p.as_str()));
        assert_eq!(o.status.code(), Some(1));
        let text = String::from_utf8(raw(&o)).unwrap();
        assert!(
            text.contains(r"unknown field `\u{1b}]0;pwned\u{7}\u{202e}`"),
            "{text}"
        );
        assert!(!text.contains(['\u{1b}', '\u{7}', '\u{202e}']), "{text}");
    }
    fs::write(&path, &good).unwrap();
    let mark = sb.root().join(format!("sessions/{sid}.p1.hw"));
    let m = fs::read_to_string(&mark).unwrap();
    fs::write(&mark, m.replacen('{', &format!("{{{evil}"), 1)).unwrap();
    let o = sb.run(&["play"], "", Some(&alice));
    assert_eq!(o.status.code(), Some(1));
    let text = String::from_utf8(raw(&o)).unwrap();
    assert!(
        text.contains("doesn't parse (unknown field `\\u{1b}]0;pwned"),
        "{text}"
    );
    assert!(!text.contains(['\u{1b}', '\u{7}', '\u{202e}']), "{text}");
}

fn plant_temp_dirs(sb: &Sandbox, sid: &str, seat: &str) {
    let dir = sb.root().join("sessions");
    for name in [
        format!("{sid}.{seat}.hw.tmp"),
        format!("{sid}.{seat}.hw.0000000000000000.tmp"),
        format!("{sid}.{seat}.hw.3f9a0c17d2e4b865.tmp"),
        format!("{sid}.json.tmp"),
        format!("{sid}.json.a1b2c3d4e5f60718.tmp"),
    ] {
        fs::create_dir(dir.join(&name)).unwrap();
        fs::write(dir.join(name).join("x"), "").unwrap();
    }
}

/// H1: directories planted at temp names can't make a reveal land silently.
/// A directory squatting the mark itself refuses before anything is appended;
/// otherwise the reveal run prints the round and the head as usual.
#[test]
fn planted_temp_dirs_cant_hide_a_reveal() {
    let sb = Sandbox::new();
    let (sid, alice, bob) = liars_dice_table(&sb);
    sb.run(&["play"], "", Some(&alice)); // 4 commit, 5 entropy
    sb.run(&["play"], "", Some(&bob)); // 6 entropy
    sb.run(&["play", "bid", "1", "2"], "", Some(&alice)); // 7
    let o = sb.run(&["play", "liar"], "", Some(&bob)); // 8 call, 9 reveal, 10 commit
    assert!(out(&o).contains("waiting on Alice's reveal"), "{}", out(&o));
    plant_temp_dirs(&sb, &sid, "p1");

    let path = sb.root().join(format!("sessions/{sid}.json"));
    let log = fs::read_to_string(&path).unwrap();
    let mark = sb.root().join(format!("sessions/{sid}.p1.hw"));
    let aside = sb.root().join("p1.hw.aside");
    fs::rename(&mark, &aside).unwrap();
    fs::create_dir(&mark).unwrap();
    let o = sb.run(&["play"], "", Some(&alice));
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(out(&o), "");
    assert_eq!(fs::read_to_string(&path).unwrap(), log);
    fs::remove_dir(&mark).unwrap();
    fs::rename(&aside, &mark).unwrap();

    let o = sb.run(&["play"], "", Some(&alice)); // 11 reveal → resolve, 12, 13
    assert!(o.status.success(), "{}", err(&o));
    assert!(out(&o).contains("Round 1 result: Bob called liar on Alice's 1×2."));
    assert!(out(&o).contains("\nhead 13:"), "{}", out(&o));
    let o = sb.run(&["verify", &sid], "", None);
    assert!(out(&o).contains("OK  14 entries  head 13:"), "{}", out(&o));
    let o = sb.run(&["play"], "", Some(&alice));
    assert!(o.status.success(), "{}", err(&o));
}

#[test]
fn planted_temp_dirs_cant_eat_a_password() {
    let sb = Sandbox::new();
    let o = sb.run(&["new", "ld"], "2\nAlice\nBob\n", None);
    let sid = after(&out(&o), "SESSION ID:").to_string();
    sb.run(&["register", &sid, "Alice"], "", None);
    plant_temp_dirs(&sb, &sid, "p2");
    let o = sb.run(&["register", &sid, "Bob"], "", None);
    assert!(o.status.success(), "{}", err(&o));
    let bob = after(&out(&o), "PASSWORD (keep secret):").to_string();
    assert!(out(&o).contains("\nhead 3:"), "{}", out(&o));
    assert!(sb.run(&["play"], "", Some(&bob)).status.success());
}

/// H3: a log rolled back past your own register says so, not "wrong password".
#[test]
fn a_rollback_past_your_register_is_named() {
    let sb = Sandbox::new();
    let o = sb.run(&["new", "ld"], "2\nAlice\nBob\n", None);
    let sid = after(&out(&o), "SESSION ID:").to_string();
    let path = sb.root().join(format!("sessions/{sid}.json"));
    let genesis_only = fs::read_to_string(&path).unwrap();
    let o = sb.run(&["register", &sid, "Alice"], "", None);
    let alice = after(&out(&o), "PASSWORD (keep secret):").to_string();
    fs::write(&path, &genesis_only).unwrap();
    let o = sb.run(&["play"], "", Some(&alice));
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(
        err(&o).trim(),
        "error: ROLLBACK: your seat p1's registration is gone from this log. Don't play on. Run 0sum verify."
    );
    let stranger = format!("{sid}-AAAAAAAAAAAAAAAAAAAAAAAAAA");
    let o = sb.run(&["play"], "", Some(&stranger));
    assert!(
        err(&o).contains("this password doesn't belong to any seat"),
        "{}",
        err(&o)
    );
}

/// D4: a refused look still shows where you are.
#[test]
fn refusals_after_the_mark_check_still_print_the_head() {
    let sb = Sandbox::new();
    let o = sb.run(&["new", "ld"], "2\nAlice\nBob\n", None);
    let sid = after(&out(&o), "SESSION ID:").to_string();
    let o = sb.run(&["register", &sid, "Alice"], "", None);
    let alice = after(&out(&o), "PASSWORD (keep secret):").to_string();
    let o = sb.run(&["play"], "", Some(&alice));
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("not everyone has registered yet"));
    assert!(
        out(&o).contains("your last move: registered (seq 1)\nhead 1:"),
        "{}",
        out(&o)
    );
    sb.run(&["register", &sid, "Bob"], "", None);
    let o = sb.run(&["play", "bid", "300", "5"], "", Some(&alice));
    assert_eq!(
        err(&o).trim(),
        "error: `300` is too big for a bid (255 at most)"
    );
    assert!(out(&o).contains("\nhead "), "{}", out(&o));
}

/// D5: an unknown game id is escaped once, not twice.
#[test]
fn an_unknown_game_is_escaped_once() {
    let sb = Sandbox::new();
    let o = sb.run(&["new", "ld"], "2\nAlice\nBob\n", None);
    let sid = after(&out(&o), "SESSION ID:").to_string();
    let path = sb.root().join(format!("sessions/{sid}.json"));
    let text = fs::read_to_string(&path).unwrap();
    let doctored = text.replacen(r#""game": "liars-dice""#, r#""game": "chess\u001b""#, 1);
    assert_ne!(doctored, text);
    fs::write(&path, doctored).unwrap();
    let o = sb.run(&["verify", &sid], "", None);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(
        out(&o).trim(),
        r"BROKEN at seq 0: unknown game `chess\u{1b}` (available: farkle, liars-dice)"
    );
}
