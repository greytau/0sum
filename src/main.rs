//! `0sum` — provably-fair CLI games. Every session is a signed, hash-chained
//! log (docs/PHASE2.md); state is a replay of it through the game's `fold`.
//! This file is the CLI plus the registry: one `match` from game id to engine.

mod engine;
mod farkle;
mod liars_dice;
mod storage;

use std::io::{self, Write};
use std::marker::PhantomData;

use clap::{Parser, Subcommand};
use rand::rngs::{OsRng, StdRng};
use rand::{RngCore, SeedableRng};

use engine::keys::{self, Password, Secrets};
use engine::log::Session;
use engine::{
    append_register, apply_chain, escape, genesis_game, head_line, hw, new_session, replay,
    replay_narrated, Broken, Chain, Ctx, Game, Table, MAX_SEATS,
};
use farkle::Farkle;
use liars_dice::LiarsDice;
use storage::{LoadError, Store};

#[derive(Parser)]
#[command(
    name = "0sum",
    version,
    about = "Provably-fair CLI games you play against each other"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create a new game session (prompts for players), print a session id.
    New {
        /// Which game: farkle, liars-dice (aliases liarsdice, ld).
        game: String,
        /// Ruleset variant (farkle: kcd [default], classic; liars-dice: classic [default], natural).
        #[arg(long)]
        ruleset: Option<String>,
    },
    /// Claim a seat by name in a session, print that player's password.
    Register { session_id: String, name: String },
    /// Play with the password in ZEROSUM_PASSWORD. No moves = show status.
    /// Farkle: `roll`, `keep <dice...>`, `bank`; moves chain: `keep 5 5 roll`.
    /// Liar's Dice: `bid <qty> <face>`, `liar`, `spot` (one per call).
    Play {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        moves: Vec<String>,
    },
    /// Replay and check a session's whole signed log. No password needed.
    Verify { session_id: String },
}

const PASSWORD_ENV: &str = "ZEROSUM_PASSWORD";
/// A5: a shell variable passed per command, never exported, so the password
/// doesn't ride along into every child process's environment.
const LOAD_HINT: &str = r#"read -rs ZP; zs() { ZEROSUM_PASSWORD="$ZP" 0sum "$@"; }"#;

fn main() {
    let cli = Cli::parse();
    let store = Store::from_env();
    if let Some(w) = store.legacy_index_warning() {
        eprintln!("{w}");
    }
    let result = store.prepare().and_then(|()| match cli.command {
        Command::New { game, ruleset } => driver(&game)?.create(&store, ruleset.as_deref()),
        Command::Register { session_id, name } => cmd_register(&store, &session_id, &name),
        Command::Play { moves } => cmd_play(&store, &moves),
        Command::Verify { session_id } => cmd_verify(&store, &session_id),
    });
    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

trait Driver {
    fn create(&self, store: &Store, ruleset: Option<&str>) -> Result<(), String>;
    fn register(&self, store: &Store, s: Session, name: &str) -> Result<(), String>;
    fn play(
        &self,
        store: &Store,
        s: Session,
        pw: &Password,
        tokens: &[String],
    ) -> Result<(), String>;
    fn verify(&self, s: &Session) -> Result<(), String>;
}

struct Engine<G>(PhantomData<G>);

fn driver(game: &str) -> Result<&'static dyn Driver, String> {
    match game.to_ascii_lowercase().as_str() {
        "farkle" => Ok(&Engine::<Farkle>(PhantomData)),
        "liars-dice" | "liarsdice" | "ld" => Ok(&Engine::<LiarsDice>(PhantomData)),
        other => Err(format!(
            "unknown game `{}` (available: farkle, liars-dice)",
            escape(other)
        )),
    }
}

fn load(store: &Store, sid: &str) -> Result<Session, String> {
    store.load(sid).map_err(|e| e.message().to_string())
}

fn refuse_broken(sid: &str, b: Broken) -> String {
    format!("session {sid} is {b} — refusing to append (see `0sum verify {sid}`)")
}

fn session_driver(s: &Session) -> Result<&'static dyn Driver, String> {
    let game = genesis_game(s).map_err(|b| refuse_broken(&s.session_id, b))?;
    driver(game)
}

fn cmd_register(store: &Store, sid: &str, name: &str) -> Result<(), String> {
    let sid = keys::parse_sid(sid)?;
    let _lock = store.lock(&sid)?;
    let s = load(store, &sid)?;
    session_driver(&s)?.register(store, s, name)
}

fn cmd_play(store: &Store, tokens: &[String]) -> Result<(), String> {
    if tokens.iter().any(|t| keys::looks_like_password(t)) {
        return Err("don't put your password on the command line — it's in your shell history now; use ZEROSUM_PASSWORD".into());
    }
    let raw = std::env::var(PASSWORD_ENV)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| {
            format!("set {PASSWORD_ENV} — in this terminal run `{LOAD_HINT}`, paste your password, then use `zs play …`")
        })?;
    let pw = Password::parse(&raw)?;
    let _lock = store.lock(pw.sid())?;
    let s = load(store, pw.sid())?;
    session_driver(&s)?.play(store, s, &pw, tokens)
}

/// Public and read-only: prints the narrative, then `OK …` or `BROKEN …` (exit 1).
fn cmd_verify(store: &Store, sid: &str) -> Result<(), String> {
    let sid = keys::parse_sid(sid)?;
    let s = match store.load(&sid) {
        Ok(s) => s,
        Err(LoadError::Corrupt(m)) => fail_verify(&format!("BROKEN: {m}")),
        Err(e) => return Err(e.message().to_string()),
    };
    let checked = genesis_game(&s)
        .map_err(|b| b.to_string())
        .and_then(|g| driver(g).map_err(|e| format!("BROKEN at seq 0: {e}")))
        .and_then(|d| d.verify(&s));
    match checked {
        Ok(()) => Ok(()),
        Err(e) => fail_verify(&e),
    }
}

fn fail_verify(line: &str) -> ! {
    println!("{line}");
    std::process::exit(1)
}

fn ctx_for(pw: &Password) -> Ctx {
    Ctx {
        secrets: Secrets::from_password(pw),
        rng: StdRng::from_entropy(),
    }
}

/// Prints the narration and returns it, for [`print_status`].
fn print_steps<G: Game>(chain: &Chain<G>, names: &[String]) -> Vec<String> {
    let mut said = Vec::new();
    for step in &chain.steps {
        for line in G::narrate(&step.before, step.author, &step.event, &step.after, names) {
            println!("{line}");
            said.push(line);
        }
    }
    said
}

/// The table. A render can open with a block the narration just printed (a
/// resolved round's result is in both); that block isn't printed twice.
fn print_status<G: Game>(t: &Table<G>, me: usize, ctx: &Ctx, said: &[String]) {
    let view = G::render(&t.state, &t.names, Some(me), Some(ctx));
    let view = match view.split_once("\n\n") {
        Some((top, rest)) if top.lines().all(|l| said.iter().any(|s| s == l)) => rest,
        _ => &view,
    };
    println!();
    println!("{view}");
}

/// A4: on every password-holding run, so a human notices a vanished move.
fn print_whereabouts<G: Game>(s: &Session, t: &Table<G>, me: usize) {
    println!();
    println!("{}", t.recap_line(me));
    println!("{}", head_line(s));
}

/// A4's check: after replay, under the lock, before any duty. `play` then
/// writes the mark at this head at once, so a mark that can't be written
/// refuses before anything is appended (H1).
fn check_mark<G: Game>(
    store: &Store,
    s: &Session,
    t: &Table<G>,
    seat: usize,
    sk: &ed25519_dalek::SigningKey,
) -> Result<(), String> {
    let raw = store.read_hw(&s.session_id, &t.seats[seat])?;
    hw::check(s, &t.seats[seat], &sk.verifying_key(), raw.as_deref())
}

/// A password with no seat: if one of this session's marks is signed by it,
/// the log was rolled back past its register (H3).
fn not_seated<G: Game>(
    store: &Store,
    s: &Session,
    t: &Table<G>,
    sk: &ed25519_dalek::SigningKey,
) -> String {
    let key = sk.verifying_key();
    for seat in &t.seats {
        if let Ok(Some(raw)) = store.read_hw(&s.session_id, seat) {
            if hw::signed_by(s, &key, &raw) {
                return format!("ROLLBACK: your seat {seat}'s registration is gone from this log. Don't play on. Run 0sum verify.");
            }
        }
    }
    format!(
        "this password doesn't belong to any seat in session {}",
        t.session_id
    )
}

/// After a save, a failed write is returned only once the run has printed what
/// it appended and the head (H1): a reveal never lands silently.
fn write_mark<G: Game>(
    store: &Store,
    s: &Session,
    t: &Table<G>,
    seat: usize,
    sk: &ed25519_dalek::SigningKey,
) -> Result<(), String> {
    store.write_hw(&s.session_id, &t.seats[seat], &hw::mark(s, sk))
}

impl<G: Game> Driver for Engine<G> {
    fn create(&self, store: &Store, ruleset: Option<&str>) -> Result<(), String> {
        let rules = G::ruleset(ruleset.unwrap_or(G::DEFAULT_RULESET))?;
        let count = prompt_usize("How many players? [2]: ", 2)?;
        if !(2..=MAX_SEATS).contains(&count) {
            return Err(format!("need 2 to {MAX_SEATS} players"));
        }
        let mut names = Vec::new();
        for i in 1..=count {
            let name = prompt_line(&format!("Player {i} name: "))?;
            names.push(match name.trim() {
                "" => format!("Player {i}"),
                n => n.to_string(),
            });
        }
        let s = loop {
            let mut nonce = [0u8; 16];
            OsRng.fill_bytes(&mut nonce);
            let sid = keys::random_crockford(keys::SID_LEN);
            let s = new_session::<G>(&sid, &rules, &names, nonce).map_err(|e| escape(&e))?;
            if store.create(&s)? {
                break s;
            }
        };
        let sid = &s.session_id;
        println!();
        println!("Created {}", G::describe(&rules));
        println!("  SESSION ID:  {sid}");
        println!();
        println!("Each player claims their seat to get a password:");
        for n in &names {
            println!("  0sum register {sid} {}", shell_quote(n));
        }
        Ok(())
    }

    fn register(&self, store: &Store, mut s: Session, name: &str) -> Result<(), String> {
        let sid = s.session_id.clone();
        let mut t = replay::<G>(&s).map_err(|b| refuse_broken(&sid, b))?;
        let seat = t
            .names
            .iter()
            .position(|n| n.eq_ignore_ascii_case(name))
            .ok_or_else(|| format!("no player named `{name}` in session {}", t.session_id))?;
        if t.keys[seat].is_some() {
            return Err(format!("`{}` has already registered", t.names[seat]));
        }
        let pw = Password::generate(&t.session_id);
        let mut ctx = ctx_for(&pw);
        let sk = ctx.secrets.signing_key();
        if store.read_hw(&sid, &t.seats[seat])?.is_some() {
            return Err(format!(
                "seat {} has a high-water mark but no registration in this log: the log was rolled back past {}'s register, or the file was planted. Run 0sum verify.",
                t.seats[seat], t.names[seat]
            ));
        }
        append_register(&mut s, &mut t, seat, &sk)?;
        let chain = if t.started() {
            apply_chain(&mut s, &mut t, seat, &[], &mut ctx, &sk)
        } else {
            Chain {
                steps: Vec::new(),
                failure: None,
            }
        };
        store.save(&s)?;
        let marked = write_mark(store, &s, &t, seat, &sk);

        println!();
        println!(
            "Registered {} in session {}, signing over: {}; seats: {}.",
            t.names[seat],
            t.session_id,
            G::describe(&t.rules),
            t.names.join(", ")
        );
        println!("  PASSWORD (keep secret):  {}", pw.expose());
        println!();
        println!(
            "Load it into this terminal: run this, paste the password at the silent prompt, Enter."
        );
        println!("  {LOAD_HINT}");
        println!("Then play with `zs play …` (plain `zs play` shows the table).");
        println!();
        let said = print_steps(&chain, &t.names);
        if t.started() {
            println!("All players in.");
            print_status(&t, seat, &ctx, &said);
        } else {
            println!("Waiting on: {}", t.unregistered().join(", "));
        }
        print_whereabouts(&s, &t, seat);
        marked?;
        chain.failure.map_or(Ok(()), Err)
    }

    fn play(
        &self,
        store: &Store,
        mut s: Session,
        pw: &Password,
        tokens: &[String],
    ) -> Result<(), String> {
        let sid = s.session_id.clone();
        let mut t = replay::<G>(&s).map_err(|b| refuse_broken(&sid, b))?;
        let mut ctx = ctx_for(pw);
        let sk = ctx.secrets.signing_key();
        let Some(me) = t.seat_of(&sk.verifying_key()) else {
            return Err(not_seated(store, &s, &t, &sk));
        };
        check_mark(store, &s, &t, me, &sk)?;
        write_mark(store, &s, &t, me, &sk)?;
        if !t.started() {
            print_whereabouts(&s, &t, me);
            return Err(format!(
                "not everyone has registered yet (waiting on: {})",
                t.unregistered().join(", ")
            ));
        }
        if let Some(line) = G::game_over(&t.state, &t.names) {
            println!("{line}");
            print_status(&t, me, &ctx, &[]);
            print_whereabouts(&s, &t, me);
            return Ok(());
        }
        let moves = if tokens.is_empty() {
            Vec::new()
        } else {
            match G::parse_moves(tokens) {
                Ok(moves) => moves,
                Err(e) => {
                    print_whereabouts(&s, &t, me);
                    return Err(e);
                }
            }
        };
        let before = s.log.len();
        let chain = apply_chain(&mut s, &mut t, me, &moves, &mut ctx, &sk);
        let appended = s.log.len() > before;
        if appended {
            store.save(&s)?;
        }
        let marked = write_mark(store, &s, &t, me, &sk);
        let said = print_steps(&chain, &t.names);
        if appended || tokens.is_empty() {
            print_status(&t, me, &ctx, &said);
        }
        print_whereabouts(&s, &t, me);
        marked?;
        chain.failure.map_or(Ok(()), Err)
    }

    fn verify(&self, s: &Session) -> Result<(), String> {
        let mut narrative = Vec::new();
        let result = replay_narrated::<G>(s, &mut narrative);
        for line in &narrative {
            println!("{line}");
        }
        result.map_err(|b| b.to_string())?;
        println!("OK  {} entries  {}", s.log.len(), head_line(s));
        Ok(())
    }
}

fn prompt_line(prompt: &str) -> Result<String, String> {
    print!("{prompt}");
    io::stdout().flush().ok();
    let mut buf = String::new();
    io::stdin()
        .read_line(&mut buf)
        .map_err(|e| format!("read input: {e}"))?;
    Ok(buf.trim_end_matches(['\n', '\r']).to_string())
}

fn prompt_usize(prompt: &str, default: usize) -> Result<usize, String> {
    let line = prompt_line(prompt)?;
    if line.trim().is_empty() {
        return Ok(default);
    }
    line.trim()
        .parse::<usize>()
        .map_err(|_| format!("`{line}` is not a number"))
}

fn shell_quote(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::testkit::PASSWORDS;
    use farkle::tests::one_keep_from_a_win;
    use farkle::Move;

    /// D9: as in phase 1, any `play` on a won Farkle game shows the result and
    /// succeeds, appending nothing.
    #[test]
    fn play_on_a_finished_farkle_game_succeeds() {
        let (mut s, mut t, mut players, seat, keep) = one_keep_from_a_win();
        let p = &mut players[seat];
        let moves = [Move::Roll, Move::Keep(keep), Move::Bank];
        assert_eq!(
            apply_chain(&mut s, &mut t, seat, &moves, &mut p.ctx, &p.sk).failure,
            None
        );
        assert!(t.state.winner.is_some());

        let root = std::env::temp_dir().join(format!("0sum-main-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let store = Store::at(root.clone());
        store.prepare().unwrap();
        assert!(store.create(&s).unwrap());
        for (i, p) in players.iter().enumerate() {
            store
                .write_hw("TEST01", &t.seats[i], &hw::mark(&s, &p.sk))
                .unwrap();
        }
        let farkle = Engine::<Farkle>(PhantomData);
        for pw in &PASSWORDS[..2] {
            let pw = Password::parse(pw).unwrap();
            for tokens in [&["roll"][..], &[], &["bank"], &["yeet"]] {
                let tokens: Vec<String> = tokens.iter().map(|t| t.to_string()).collect();
                let loaded = store.load("TEST01").ok().unwrap();
                assert_eq!(farkle.play(&store, loaded, &pw, &tokens), Ok(()));
            }
        }
        assert_eq!(store.load("TEST01").ok().unwrap().log.len(), s.log.len());
        std::fs::remove_dir_all(&root).unwrap();
    }
}
