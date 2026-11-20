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
        self.run_with(args, stdin, password, &[])
    }

    fn run_with(
        &self,
        args: &[&str],
        stdin: &str,
        password: Option<&str>,
        env: &[(&str, &str)],
    ) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_0sum"));
        cmd.args(args)
            .env("XDG_CONFIG_HOME", &self.0)
            .env("HOME", &self.0)
            .env_remove("ZEROSUM_PASSWORD")
            .env_remove("ZEROSUM_FORMAT")
            .env_remove("NO_COLOR")
            .envs(env.iter().copied())
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
        r"BROKEN at seq 0: unknown game `chess\u{1b}` (available: battleship, farkle, liars-dice)"
    );
}

/// `C3-G3` → C3 D3 E3 F3 G3, from a ledger `your fleet:` line.
fn ledger_squares(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    for part in line.trim_start_matches("your fleet: ").split(" · ") {
        let span = part.split(' ').nth(1).unwrap();
        let (a, b) = span.split_once('-').unwrap();
        let rc = |s: &str| (s.as_bytes()[0], s[1..].parse::<u8>().unwrap());
        let ((r0, c0), (r1, c1)) = (rc(a), rc(b));
        for r in r0..=r1 {
            for c in c0..=c1 {
                out.push(format!("{}{c}", r as char));
            }
        }
    }
    out
}

/// Two terminals, two passwords: Alice places by hand, Bob places at random
/// and plays from the ledger. Alice reads Bob's own ledger (the test is the
/// cheat here, not the binary) and sinks him; her next run reveals.
#[test]
fn battleship_two_terminals_to_a_winner() {
    let sb = Sandbox::new();
    let o = sb.run(&["new", "bs"], "Alice\nBob\n", None);
    assert!(o.status.success(), "{}", err(&o));
    assert!(out(&o).contains("Battleship is a 2-player game."));
    assert!(!out(&o).contains("How many players"));
    let sid = after(&out(&o), "SESSION ID:").to_string();
    let o = sb.run(&["register", &sid, "Alice"], "", None);
    assert!(out(&o).contains(r#"read -rs ZP; zs() { ZEROSUM_PASSWORD="$ZP" 0sum "$@"; }"#));
    let alice = after(&out(&o), "PASSWORD (keep secret):").to_string();
    let o = sb.run(&["register", &sid, "Bob", "--format", "LEDGER"], "", None);
    assert!(
        out(&o).contains(
            r#"read -rs ZP; zs() { ZEROSUM_PASSWORD="$ZP" ZEROSUM_FORMAT=ledger 0sum "$@"; }"#
        ),
        "{}",
        out(&o)
    );
    assert!(out(&o).contains("placing · you vs Alice · enemy placed 0 of 5, not locked"));
    let bob = after(&out(&o), "PASSWORD (keep secret):").to_string();
    let ledger = [("ZEROSUM_FORMAT", "ledger")];
    let alice_play = |args: &[&str]| {
        let mut a = vec!["play"];
        a.extend(args);
        let o = sb.run(&a, "", Some(&alice));
        assert!(o.status.success(), "{}{}", out(&o), err(&o));
        out(&o)
    };
    let bob_play = |args: &[&str]| {
        let mut a = vec!["play"];
        a.extend(args);
        let o = sb.run_with(&a, "", Some(&bob), &ledger);
        assert!(o.status.success(), "{}{}", out(&o), err(&o));
        out(&o)
    };

    let text = alice_play(&[
        "place",
        "carrier",
        "A1",
        "h",
        "battleship",
        "C1",
        "h",
        "cruiser",
        "E1",
        "h",
        "submarine",
        "G1",
        "h",
        "destroyer",
        "I1",
        "h",
        "ready",
    ]);
    assert!(text.contains("Alice locked the fleet"), "{text}");
    assert!(
        text.contains("--- battleship: Alice vs Bob, placing ---"),
        "{text}"
    );
    let text = bob_play(&["place", "auto", "ready"]);
    assert!(
        text.contains("turn 1 · you vs Alice · enemy's shot")
            && !text.contains("Alice fires first"),
        "{text}"
    );
    let fleet = text
        .lines()
        .find(|l| l.starts_with("your fleet: "))
        .unwrap();
    let targets = ledger_squares(fleet);
    assert_eq!(targets.len(), 17);
    let water: Vec<String> = ["B", "D", "F", "H", "J"]
        .iter()
        .flat_map(|r| (1..=10).map(move |c| format!("{r}{c}")))
        .collect();

    for (n, sq) in targets.iter().enumerate() {
        let text = alice_play(&[sq]);
        assert!(text.contains(&format!("Alice fires at {sq}")), "{text}");
        let shot = [water[n].as_str()];
        let text = bob_play(if n < 16 { &shot } else { &[] });
        assert!(
            text.lines()
                .any(|l| l.starts_with("last: ") && l.contains(&format!(" · enemy {sq} "))),
            "{text}"
        );
    }
    let text = bob_play(&[]);
    assert!(
        text.starts_with("\nreveal · enemy sank your fleet · waiting on enemy's final reveal\n"),
        "{text}"
    );
    let text = alice_play(&[]);
    assert!(
        text.contains("Alice won: sank Bob's fleet; Alice's fleet verified"),
        "{text}"
    );
    let path = sb.root().join(format!("sessions/{sid}.json"));
    let log = fs::read_to_string(&path).unwrap();
    for format in ["logbook", "bridge"] {
        let o = sb.run(&["play", "B5", "--format", format], "", Some(&alice));
        assert!(o.status.success());
        assert!(
            out(&o).starts_with("Alice won: sank Bob's fleet; Alice's fleet verified\n"),
            "{format}: {}",
            out(&o)
        );
    }
    assert_eq!(fs::read_to_string(&path).unwrap(), log);

    let o = sb.run(&["verify", &sid], "", None);
    assert!(o.status.success(), "{}", out(&o));
    let text = out(&o);
    assert!(text.contains("Alice reveals: carrier A1 h, battleship C1 h, cruiser E1 h, submarine G1 h, destroyer I1 h — legal, every answer matched"), "{text}");
    assert!(text.lines().last().unwrap().starts_with("OK  "), "{text}");
}

/// R3: `--format` is a request (unknown = error, nothing written); the env
/// is a preference (unknown = the game's default, silently).
#[test]
fn render_formats_flag_errors_env_falls_back() {
    let sb = Sandbox::new();
    let o = sb.run(&["new", "battleship"], "Alice\nBob\n", None);
    let sid = after(&out(&o), "SESSION ID:").to_string();
    let path = sb.root().join(format!("sessions/{sid}.json"));
    let before = fs::read_to_string(&path).unwrap();
    let o = sb.run(&["register", &sid, "Alice", "--format", "harbor"], "", None);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(
        err(&o).trim(),
        "error: battleship has no `harbor` format (bridge, ledger, logbook)"
    );
    assert!(!out(&o).contains("PASSWORD"));
    assert_eq!(fs::read_to_string(&path).unwrap(), before);
    let o = sb.run(&["register", &sid, "Alice"], "", None);
    let alice = after(&out(&o), "PASSWORD (keep secret):").to_string();
    sb.run(&["register", &sid, "Bob"], "", None);

    let o = sb.run(
        &["play", "--format", "x\u{1b}[2J", "place", "auto"],
        "",
        Some(&alice),
    );
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(
        err(&o).trim(),
        r"error: battleship has no `x\u{1b}[2J` format (bridge, ledger, logbook)"
    );
    let log = fs::read_to_string(&path).unwrap();
    let o = sb.run_with(&["play"], "", Some(&alice), &[("ZEROSUM_FORMAT", "harbor")]);
    assert!(o.status.success());
    assert!(
        out(&o).contains("--- battleship: Alice vs Bob, placing ---"),
        "{}",
        out(&o)
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), log);
    let o = sb.run_with(
        &["play", "--format", "bridge"],
        "",
        Some(&alice),
        &[("NO_COLOR", "1")],
    );
    assert!(out(&o).contains("╭─ 0sum · battleship"), "{}", out(&o));
    assert!(!out(&o).contains('\u{1b}'));
    let o = sb.run(&["play", "--format", "bridge"], "", Some(&alice));
    assert!(
        out(&o).contains("\u{1b}[1m0sum · battleship\u{1b}[0m"),
        "{}",
        out(&o)
    );
    let o = sb.run_with(&["play"], "", Some(&alice), &[("ZEROSUM_FORMAT", "ledger")]);
    assert!(
        out(&o).starts_with("\nplacing · you vs Bob · "),
        "{}",
        out(&o)
    );

    // Farkle has one format: the env is ignored, the flag must name it.
    let o = sb.run(&["new", "farkle"], "2\nAlice\nBob\n", None);
    let fsid = after(&out(&o), "SESSION ID:").to_string();
    let o = sb.run(
        &["register", &fsid, "Alice", "--format", "ledger"],
        "",
        None,
    );
    assert_eq!(
        err(&o).trim(),
        "error: farkle has no `ledger` format (default)"
    );
    let o = sb.run(&["register", &fsid, "Alice"], "", None);
    let fa = after(&out(&o), "PASSWORD (keep secret):").to_string();
    let o = sb.run(&["register", &fsid, "Bob"], "", None);
    let plain = out(&o);
    assert!(plain.contains("Alice's turn — roll: zs play roll"));
    let looks = |env: &[(&str, &str)]| {
        let o = sb.run_with(&["play"], "", Some(&fa), env);
        assert!(o.status.success(), "{}", err(&o));
        out(&o)
    };
    assert_eq!(looks(&[("ZEROSUM_FORMAT", "ledger")]), looks(&[]));
}

/// A battleship table with both seats registered: (SID, Alice's, Bob's password).
fn battleship_table(sb: &Sandbox, new: &[&str]) -> (String, String, String) {
    let mut args = vec!["new", "battleship"];
    args.extend(new);
    let o = sb.run(&args, "Alice\nBob\n", None);
    assert!(o.status.success(), "{}", err(&o));
    let sid = after(&out(&o), "SESSION ID:").to_string();
    let mut pw = Vec::new();
    for name in ["Alice", "Bob"] {
        let o = sb.run(&["register", &sid, name], "", None);
        assert!(o.status.success(), "{}", err(&o));
        pw.push(after(&out(&o), "PASSWORD (keep secret):").to_string());
    }
    let bob = pw.pop().unwrap();
    (sid, pw.pop().unwrap(), bob)
}

/// Every refusal through the real binary: exit 1, the reason on stderr, and
/// not one byte appended to the log.
#[test]
fn battleship_illegal_moves_are_refused_and_append_nothing() {
    let sb = Sandbox::new();
    let log = |pw: &str| {
        let sid = pw.split('-').next().unwrap();
        fs::read_to_string(sb.root().join(format!("sessions/{sid}.json"))).unwrap()
    };
    let refused = |pw: &str, args: &[&str]| -> String {
        let before = log(pw);
        let mut a = vec!["play"];
        a.extend(args);
        let o = sb.run(&a, "", Some(pw));
        assert_eq!(o.status.code(), Some(1), "{args:?}: {}", out(&o));
        assert_eq!(log(pw), before, "{args:?} appended");
        err(&o).trim().strip_prefix("error: ").unwrap().to_string()
    };
    let played = |pw: &str, args: &[&str]| {
        let mut a = vec!["play"];
        a.extend(args);
        let o = sb.run(&a, "", Some(pw));
        assert!(o.status.success(), "{args:?}: {}", err(&o));
    };
    let usage = "place takes <ship> <square> <h|v>, e.g. `place carrier A1 h`, or `place auto`";
    let moves =
        "(try: place <ship> <square> <h|v> | place auto | ready | fire <square> | <square>)";

    let (_, alice, bob) = battleship_table(&sb, &[]);
    assert_eq!(refused(&bob, &["B5"]), "the battle hasn't started");
    assert_eq!(
        refused(&alice, &["ready"]),
        "place the carrier, battleship, cruiser, submarine and destroyer first (or place auto)"
    );
    for (args, why) in [
        (
            &["place", "carrier", "A8", "h"][..],
            "the carrier doesn't fit at A8 across (5 long; the board ends at column 10)",
        ),
        (
            &["place", "carrier", "J1", "v"],
            "the carrier doesn't fit at J1 down (5 long; the board ends at row J)",
        ),
        (
            &["place", "carrier", "K1", "h"],
            "`K1` isn't on the board (rows A-J, columns 1-10)",
        ),
        (
            &["place", "x", "A1", "h"],
            "unknown ship `x` (carrier, battleship, cruiser, submarine, destroyer)",
        ),
        (
            &["place", "c", "A1", "h"],
            "`c` could be carrier or cruiser — type more letters",
        ),
        (&["place"], usage),
        (&["place", "carrier", "A1"], usage),
        (&["place", "carrier", "A1", "diagonal"], usage),
    ] {
        assert_eq!(refused(&alice, args), why, "{args:?}");
    }
    played(&alice, &["place", "carrier", "A1", "h"]);
    assert_eq!(
        refused(&alice, &["place", "battleship", "A3", "v"]),
        "the battleship at A3 v overlaps your carrier at A3"
    );
    assert_eq!(
        refused(&alice, &["ready"]),
        "place the battleship, cruiser, submarine and destroyer first (or place auto)"
    );
    played(
        &alice,
        &["place", "carrier", "B1", "h", "place", "auto", "ready"],
    );
    for args in [&["place", "carrier", "A1", "h"][..], &["ready"], &["B5"]] {
        assert_eq!(
            refused(&alice, args),
            "your fleet is locked; waiting on Bob to place theirs"
        );
    }
    played(&bob, &["place", "auto", "ready"]);

    assert_eq!(refused(&bob, &["B5"]), "it's Alice's turn, not yours");
    assert_eq!(
        refused(&alice, &["fire"]),
        "fire takes a square, e.g. `fire B5`"
    );
    for sq in ["K1", "A11", "A0"] {
        assert_eq!(
            refused(&alice, &[sq]),
            format!("`{sq}` isn't on the board (rows A-J, columns 1-10)")
        );
    }
    assert_eq!(
        refused(&alice, &["--format", "ledger", "B5", "--format", "ledger"]),
        "--format given twice"
    );
    assert_eq!(
        refused(&alice, &["B5", "--format=ledger", "--format", "bridge"]),
        "--format given twice"
    );
    assert_eq!(
        refused(&alice, &["B5", "--format"]),
        "--format takes a format id"
    );
    assert_eq!(
        refused(&alice, &["B5", "--format", "harbor"]),
        "battleship has no `harbor` format (bridge, ledger, logbook)"
    );
    assert_eq!(
        refused(&alice, &["fire", "B5", "--formats"]),
        format!("unknown move `--formats` {moves}")
    );
    played(&alice, &["b5", "--format", "LEDGER"]);
    assert_eq!(
        refused(&alice, &["B6"]),
        "waiting on Bob's answer to the shot at B5"
    );
    played(&bob, &[]);
    for args in [&["place", "auto"][..], &["ready"]] {
        refused(&bob, args);
    }
    played(&bob, &["A1"]);
    played(&alice, &[]);
    for sq in ["B5", "b5", "fire B5"] {
        let args: Vec<&str> = sq.split(' ').collect();
        assert_eq!(refused(&alice, &args), "you already fired at B5");
    }

    let (_, alice, _) = battleship_table(&sb, &["--ruleset", "notouch"]);
    played(&alice, &["place", "carrier", "A1", "h"]);
    assert_eq!(
        refused(&alice, &["place", "cruiser", "B6", "h"]),
        "the cruiser at B6 h touches your carrier (ruleset notouch keeps ships apart)"
    );
}

/// Mid-battle, every file under the seat's XDG/HOME holds no password and no
/// placement but the one sunk ship's, and every ledger is free of escapes.
#[test]
fn battleship_disk_holds_no_unopened_placement() {
    let sb = Sandbox::new();
    let (sid, alice, bob) = battleship_table(&sb, &[]);
    let play = |pw: &str, args: &[&str]| -> String {
        let mut a = vec!["play"];
        a.extend(args);
        let o = sb.run_with(&a, "", Some(pw), &[("ZEROSUM_FORMAT", "ledger")]);
        assert!(o.status.success(), "{args:?}: {}", err(&o));
        assert!(!out(&o).contains('\u{1b}'), "{}", out(&o));
        out(&o)
    };
    play(&alice, &["place", "auto", "ready"]);
    let text = play(&bob, &["place", "auto", "ready"]);
    let fleet = text
        .lines()
        .find(|l| l.starts_with("your fleet: "))
        .unwrap();
    let destroyer = fleet.split(" · ").last().unwrap();
    let squares = ledger_squares(destroyer);
    play(&alice, &[&squares[0]]);
    play(&bob, &["J10"]);
    play(&alice, &[&squares[1]]);
    let text = play(&bob, &["J9"]);
    let fleet = text
        .lines()
        .find(|l| l.starts_with("your fleet: "))
        .unwrap();
    let last = fleet.split(" · ").last().unwrap();
    assert!(
        last.starts_with("destroyer ") && last.ends_with(" SUNK"),
        "{text}"
    );
    play(&alice, &[]);

    let mut files = Vec::new();
    let mut dirs = vec![sb.0.clone()];
    while let Some(d) = dirs.pop() {
        for e in fs::read_dir(d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                dirs.push(p);
            } else {
                files.push(p);
            }
        }
    }
    let mut names: Vec<String> = files
        .iter()
        .map(|p| p.strip_prefix(&sb.0).unwrap().display().to_string())
        .collect();
    names.sort();
    let s = format!("0sum/sessions/{sid}");
    assert_eq!(
        names,
        [".json", ".lock", ".p1.hw", ".p2.hw"].map(|x| format!("{s}{x}"))
    );
    let mut placements = Vec::new();
    for p in &files {
        let raw = fs::read_to_string(p).unwrap();
        for pw in [&alice, &bob] {
            assert!(!raw.contains(pw.split('-').nth(1).unwrap()), "{p:?}");
        }
        if raw.is_empty() {
            continue;
        }
        let mut todo = vec![serde_json::from_str::<serde_json::Value>(&raw).unwrap()];
        while let Some(v) = todo.pop() {
            match v {
                serde_json::Value::Object(m) => {
                    assert!(!m.contains_key("reveal") && !m.contains_key("b"), "{p:?}");
                    if m.contains_key("row") {
                        placements.push(serde_json::Value::Object(m.clone()));
                    }
                    todo.extend(m.into_iter().map(|(_, v)| v));
                }
                serde_json::Value::Array(a) => todo.extend(a),
                _ => {}
            }
        }
    }
    let (row, col) = (squares[0].as_bytes()[0] - b'A', &squares[0][1..]);
    let dir = if squares[0][..1] == squares[1][..1] {
        "h"
    } else {
        "v"
    };
    let col: u8 = col.parse().unwrap();
    assert_eq!(
        placements,
        [serde_json::json!({ "row": row, "col": col - 1, "dir": dir })]
    );
    let o = sb.run(&["verify", &sid], "", None);
    assert!(o.status.success(), "{}", out(&o));
}

/// R3 at the edges: an env value is never echoed, an empty NO_COLOR is unset,
/// the flag beats the env, and a ledger from the flag has no escapes either.
#[test]
fn battleship_format_edges() {
    let sb = Sandbox::new();
    let (_, alice, _) = battleship_table(&sb, &[]);
    let look = |args: &[&str], env: &[(&str, &str)]| {
        let mut a = vec!["play"];
        a.extend(args);
        let o = sb.run_with(&a, "", Some(&alice), env);
        assert!(o.status.success(), "{}", err(&o));
        assert_eq!(err(&o), "");
        out(&o)
    };
    let text = look(&[], &[("ZEROSUM_FORMAT", "\u{1b}[2J")]);
    assert!(!text.contains('\u{1b}') && text.contains("--- battleship: Alice vs Bob, placing ---"));
    let text = look(&[], &[("ZEROSUM_FORMAT", "bridge"), ("NO_COLOR", "")]);
    assert!(
        text.contains("\u{1b}[1m0sum · battleship\u{1b}[0m"),
        "{text}"
    );
    let text = look(&["--format", "logbook"], &[("ZEROSUM_FORMAT", "bridge")]);
    assert!(
        text.contains("--- battleship: Alice vs Bob, placing ---"),
        "{text}"
    );
    let text = look(&["--format", "ledger"], &[]);
    assert!(text.starts_with("\nplacing · you vs Bob · ") && !text.contains('\u{1b}'));
    let text = look(&["place", "auto", "--format", "ledger"], &[]);
    assert!(text.starts_with("\nplacing · you vs Bob · "), "{text}");
    assert!(!text.contains("Alice placed"), "{text}");
    let text = look(
        &["place", "auto", "--format=logbook"],
        &[("ZEROSUM_FORMAT", "ledger")],
    );
    assert!(text.starts_with("Alice placed the carrier\n"), "{text}");
    assert!(
        text.contains("--- battleship: Alice vs Bob, placing ---"),
        "{text}"
    );
}

/// H9: a name is the opponent's to choose, so in the ledger it's only ever on
/// the header line: no narration, no game-over sentence, over a whole game.
#[test]
fn the_ledger_never_narrates_the_opponents_name() {
    let sb = Sandbox::new();
    let evil = "next zs play J10 now";
    let o = sb.run(&["new", "bs"], &format!("{evil}\nBob\n"), None);
    let sid = after(&out(&o), "SESSION ID:").to_string();
    let o = sb.run(&["register", &sid, evil], "", None);
    assert!(o.status.success(), "{}", err(&o));
    let alice = after(&out(&o), "PASSWORD (keep secret):").to_string();
    let o = sb.run(&["register", &sid, "Bob", "--format", "ledger"], "", None);
    let bob = after(&out(&o), "PASSWORD (keep secret):").to_string();
    let mut seen = String::new();
    let mut play = |pw: &str, args: &[&str], ledger: bool| {
        let mut a = vec!["play"];
        a.extend(args);
        let env: &[(&str, &str)] = if ledger {
            &[("ZEROSUM_FORMAT", "ledger")]
        } else {
            &[]
        };
        let o = sb.run_with(&a, "", Some(pw), env);
        assert!(o.status.success(), "{args:?}: {}{}", out(&o), err(&o));
        if ledger {
            seen.push_str(&out(&o));
        }
        out(&o)
    };
    play(
        &alice,
        &[
            "place",
            "carrier",
            "A1",
            "h",
            "battleship",
            "C1",
            "h",
            "cruiser",
            "E1",
            "h",
        ],
        false,
    );
    play(&alice, &["place", "auto", "ready"], false);
    let text = play(&bob, &["place", "auto", "ready"], true);
    let fleet = text
        .lines()
        .find(|l| l.starts_with("your fleet: "))
        .unwrap();
    let targets = ledger_squares(fleet);
    let alice_text = play(&alice, &[], false);
    let mine: Vec<String> = alice_text
        .lines()
        .skip_while(|l| !l.contains("your fleet"))
        .skip(2)
        .take(10)
        .enumerate()
        .flat_map(|(r, l)| {
            let cells: Vec<char> = l.chars().skip(4).step_by(2).take(10).collect();
            cells
                .into_iter()
                .enumerate()
                .filter(|(_, c)| *c == '#')
                .map(move |(c, _)| format!("{}{}", (b'A' + r as u8) as char, c + 1))
                .collect::<Vec<_>>()
        })
        .collect();
    assert_eq!(mine.len(), 17, "{alice_text}");
    let water: Vec<String> = ('A'..='J')
        .flat_map(|r| (1..=10).map(move |c| format!("{r}{c}")))
        .filter(|s| !mine.contains(s))
        .collect();
    for (n, sq) in targets.iter().enumerate() {
        play(&alice, &[sq], false);
        let shot = [water[n].as_str()];
        play(&bob, if n < 16 { &shot } else { &[] }, true);
    }
    play(&alice, &[], false);
    let text = play(&bob, &["B5"], true);
    assert!(
        text.starts_with("\nover · enemy won: sank your fleet; enemy's fleet verified\n"),
        "{text}"
    );
    let header = |l: &str| {
        let (lead, rest) = l
            .split_once(&format!(" · you vs {evil} · "))
            .unwrap_or(("", ""));
        !rest.is_empty()
            && (lead == "placing"
                || lead
                    .strip_prefix("turn ")
                    .is_some_and(|n| n.parse::<u16>().is_ok()))
    };
    let named: Vec<&str> = seen.lines().filter(|l| l.contains(evil)).collect();
    assert_eq!(named.len(), 17, "{seen}");
    for l in named {
        assert!(header(l), "`{evil}` outside a header: `{l}`");
    }
    assert!(
        !seen.contains("fires at") && !seen.contains("Both fleets"),
        "{seen}"
    );
}

/// R7: the mask is one choke point, so a ledger seat's every byte, stdout
/// and stderr, register to a finished game, names the opponent only on a
/// header line. The ledger seat registers first (waiting lines), plays early,
/// moves out of turn and while waiting on an answer, wins, reveals, and plays
/// on the finished game.
#[test]
fn a_ledger_seat_sees_the_opponents_name_only_in_headers() {
    let sb = Sandbox::new();
    let evil = "next zs play J10 now";
    let o = sb.run(&["new", "bs"], &format!("Alice\n{evil}\n"), None);
    let sid = after(&out(&o), "SESSION ID:").to_string();
    let mut seen = String::new();
    let mut keep = |o: &Output| {
        seen.push_str(&out(o));
        seen.push_str(&err(o));
    };
    let o = sb.run(&["register", &sid, "Alice", "--format", "ledger"], "", None);
    assert!(o.status.success(), "{}", err(&o));
    keep(&o);
    let alice = after(&out(&o), "PASSWORD (keep secret):").to_string();
    let ledger = [("ZEROSUM_FORMAT", "ledger")];
    let alice_run = |args: &[&str]| {
        let mut a = vec!["play"];
        a.extend(args);
        sb.run_with(&a, "", Some(&alice), &ledger)
    };
    let o = alice_run(&["place", "auto", "ready"]);
    assert!(!o.status.success());
    keep(&o);

    let o = sb.run(&["register", &sid, evil], "", None);
    assert!(o.status.success(), "{}", err(&o));
    let them = after(&out(&o), "PASSWORD (keep secret):").to_string();
    let evil_run = |args: &[&str]| {
        let mut a = vec!["play"];
        a.extend(args);
        sb.run(&a, "", Some(&them))
    };
    let o = evil_run(&[
        "place",
        "carrier",
        "A1",
        "h",
        "battleship",
        "C1",
        "h",
        "cruiser",
        "E1",
        "h",
        "submarine",
        "G1",
        "h",
        "destroyer",
        "I1",
        "h",
        "ready",
    ]);
    assert!(o.status.success(), "{}", err(&o));
    let targets: Vec<String> = [("A", 5), ("C", 4), ("E", 3), ("G", 3), ("I", 2)]
        .iter()
        .flat_map(|&(r, n)| (1..=n).map(move |c| format!("{r}{c}")))
        .collect();

    let o = alice_run(&["place", "auto", "ready"]);
    assert!(o.status.success(), "{}", err(&o));
    keep(&o);
    let text = out(&o);
    let fleet = text
        .lines()
        .find(|l| l.starts_with("your fleet: "))
        .unwrap();
    let mine = ledger_squares(fleet);
    let water: Vec<String> = ('A'..='J')
        .flat_map(|r| (1..=10).map(move |c| format!("{r}{c}")))
        .filter(|s| !mine.contains(s))
        .collect();

    let must = |o: &Output, ok: bool| {
        assert_eq!(o.status.success(), ok, "{}{}", out(o), err(o));
    };
    for (n, sq) in targets.iter().enumerate() {
        let o = alice_run(&[sq]);
        must(&o, true);
        keep(&o);
        if n == 0 {
            let o = alice_run(&[&targets[1]]);
            must(&o, false);
            keep(&o);
            must(&evil_run(&[]), true);
            let o = alice_run(&[&targets[1]]);
            must(&o, false);
            keep(&o);
            must(&evil_run(&[&water[0]]), true);
            let o = evil_run(&[&water[1]]);
            must(&o, false);
            assert!(err(&o).contains("waiting on Alice's answer"), "{}", err(&o));
        } else if n < 16 {
            must(&evil_run(&[&water[n]]), true);
        } else {
            must(&evil_run(&[]), true);
        }
    }
    let o = alice_run(&[]);
    must(&o, true);
    keep(&o);
    let o = alice_run(&["B5"]);
    must(&o, true);
    keep(&o);
    assert!(
        out(&o).starts_with("\nover · you won: sank enemy's fleet; your fleet verified\n"),
        "{}",
        out(&o)
    );

    let header = |l: &str| {
        let (lead, rest) = l
            .split_once(&format!(" · you vs {evil} · "))
            .unwrap_or(("", ""));
        !rest.is_empty()
            && (lead == "placing"
                || lead
                    .strip_prefix("turn ")
                    .is_some_and(|n| n.parse::<u16>().is_ok()))
    };
    for l in seen.lines().filter(|l| l.contains(evil)) {
        assert!(header(l), "`{evil}` outside a header: `{l}`");
    }
    let count = |p: &dyn Fn(&str) -> bool| seen.lines().filter(|l| p(l)).count();
    let sources = [
        (
            "register",
            count(&|l| {
                l.starts_with(&format!("Registered Alice in session {sid}, "))
                    && l.ends_with("; seats: Alice, enemy.")
            }),
        ),
        (
            "waiting",
            count(&|l| {
                l == "Waiting on: enemy"
                    || l == "error: not everyone has registered yet (waiting on: enemy)"
                    || l.starts_with("error: waiting on enemy's answer to the shot at ")
            }),
        ),
        (
            "out of turn",
            count(&|l| l == "error: it's enemy's turn, not yours"),
        ),
        ("over", count(&|l| l.starts_with("over · "))),
        ("header", count(&|l| header(l))),
    ];
    eprintln!("scanned {} lines: {sources:?}", seen.lines().count());
    for (source, n) in sources {
        assert!(n > 0, "no {source} line:\n{seen}");
    }
}
