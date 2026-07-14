//! `0sum` — provably-fair CLI games. Phase 1: hotseat 2-player Farkle.
//!
//! No crypto yet (issue #1's tamper-evident core is a later chunk). State is a
//! plain JSON session file shaped as an append-only move log; score and turn
//! are projections over it (see [`session`]). Rules are swappable configs
//! decoupled from mechanics (see [`ruleset`] / [`game`], issue #2).

mod game;
mod ruleset;
mod scoring;
mod session;
mod storage;

use std::io::{self, Write};

use clap::{Parser, Subcommand};
use rand::rngs::StdRng;
use rand::SeedableRng;

use game::{Farkle, Game, Move};
use ruleset::RuleSet;
use session::{GameState, Payload, Phase, RosterEntry, Session};
use storage::IndexEntry;

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
        /// Which game (currently: farkle).
        game: String,
        /// Ruleset variant.
        #[arg(long, default_value = "kcd")]
        ruleset: String,
    },
    /// Claim a seat by name in a session, print that player's password.
    Register { session_id: String, name: String },
    /// Make a move: `roll`, `keep <dice...>`, or `bank`.
    Play {
        password: String,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        r#move: Vec<String>,
    },
}

fn main() {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::New { game, ruleset } => cmd_new(&game, &ruleset),
        Command::Register { session_id, name } => cmd_register(&session_id, &name),
        Command::Play { password, r#move } => cmd_play(&password, &r#move),
    };
    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn cmd_new(game: &str, ruleset_name: &str) -> Result<(), String> {
    if !game.eq_ignore_ascii_case("farkle") {
        return Err(format!("unknown game `{game}` (only `farkle` for now)"));
    }
    let ruleset = RuleSet::by_name(ruleset_name)?;

    let count = prompt_usize("How many players? [2]: ", 2)?;
    if count < 2 {
        return Err("need at least 2 players".into());
    }

    let mut roster = Vec::new();
    for i in 1..=count {
        let name = prompt_line(&format!("Player {i} name: "))?;
        let name = if name.trim().is_empty() {
            format!("Player {i}")
        } else {
            name.trim().to_string()
        };
        roster.push(RosterEntry {
            player_id: format!("p{i}"),
            name,
        });
    }

    let session_id = storage::gen_session_id();
    let genesis = Payload::SessionCreated {
        game: "farkle".into(),
        ruleset: ruleset.clone(),
        roster: roster.clone(),
    };
    let session = Session::new(session_id.clone(), genesis);
    storage::save_session(&session)?;

    println!();
    println!(
        "Created Farkle session (ruleset: {}, target: {})",
        ruleset.name, ruleset.target_score
    );
    println!("  SESSION ID:  {session_id}");
    println!();
    println!("Each player claims their seat to get a password:");
    for r in &roster {
        println!("  0sum register {session_id} {}", shell_quote(&r.name));
    }
    Ok(())
}

fn cmd_register(session_id: &str, name: &str) -> Result<(), String> {
    let mut session = storage::load_session(session_id)?;
    let state = session.project();

    let seat = state
        .roster
        .iter()
        .find(|r| r.name.eq_ignore_ascii_case(name))
        .ok_or_else(|| format!("no player named `{name}` in session {session_id}"))?
        .clone();

    if state.registered.contains(&seat.player_id) {
        return Err(format!("`{}` has already registered", seat.name));
    }

    let password = storage::gen_password();
    session.append(
        Some(seat.player_id.clone()),
        Payload::PlayerRegistered {
            player_id: seat.player_id.clone(),
        },
    );
    storage::save_session(&session)?;
    storage::add_password(
        &password,
        IndexEntry {
            session_id: session_id.to_string(),
            player_id: seat.player_id.clone(),
        },
    )?;

    let state = session.project();
    println!();
    println!("Registered {} in session {session_id}.", seat.name);
    println!("  PASSWORD (keep secret):  {password}");
    println!();
    if state.started {
        println!(
            "All players in. {} plays first:",
            state.current_player().name
        );
        println!("  0sum play {password} roll");
    } else {
        let waiting: Vec<&str> = state
            .roster
            .iter()
            .filter(|r| !state.registered.contains(&r.player_id))
            .map(|r| r.name.as_str())
            .collect();
        println!("Waiting on: {}", waiting.join(", "));
    }
    Ok(())
}

fn cmd_play(password: &str, move_tokens: &[String]) -> Result<(), String> {
    let entry = storage::lookup_password(password).ok_or("unknown password (did you register?)")?;
    let mut session = storage::load_session(&entry.session_id)?;
    let state = session.project();

    if let Some(winner) = &state.winner {
        println!("Game over — {} won.", state.name_of(winner));
        render(&state);
        return Ok(());
    }
    if !state.started {
        return Err("not everyone has registered yet".into());
    }

    let farkle = Farkle;
    if state.game != farkle.id() {
        return Err(format!("session is `{}`, not farkle", state.game));
    }

    let you = &entry.player_id;
    if &state.current_player().player_id != you {
        return Err(format!(
            "it's {}'s turn, not yours",
            state.current_player().name
        ));
    }

    let mv = farkle.parse_move(move_tokens)?;
    let mut rng = StdRng::from_entropy();
    let payloads = farkle.apply(&state, &mv, &mut rng)?;

    let mut busted = false;
    let mut banked: Option<u32> = None;
    for p in &payloads {
        match p {
            Payload::Busted { .. } => busted = true,
            Payload::Banked { points } => banked = Some(*points),
            _ => {}
        }
        session.append(Some(you.clone()), p.clone());
    }
    storage::save_session(&session)?;

    let after = session.project();

    if let Some(Payload::Rolled { dice }) = payloads.first() {
        println!("{} rolled: {}", state.current_player().name, fmt_dice(dice));
    }
    if let Move::Keep(_) = mv {
        if let Some(Payload::Kept { points, .. }) = payloads.first() {
            println!("Kept for {points} — turn total now {}.", after.turn_total);
        }
    }
    if busted {
        println!(
            "BUST! No scoring dice. {} loses this turn.",
            state.current_player().name
        );
    }
    if let Some(points) = banked {
        println!(
            "{} banked {points} — total {}.",
            state.current_player().name,
            after.totals[you]
        );
    }

    println!();
    render(&after);

    if let Some(winner) = &after.winner {
        println!();
        println!(">>> {} WINS! <<<", after.name_of(winner));
    } else {
        println!();
        println!("{}", next_hint(&after));
    }
    Ok(())
}

fn render(state: &GameState) {
    println!("--- scores (target {}) ---", state.ruleset.target_score);
    let current = state.winner.is_none();
    for r in &state.roster {
        let marker = if current && r.player_id == state.current_player().player_id {
            "*"
        } else {
            " "
        };
        println!("  {marker} {:<12} {}", r.name, state.totals[&r.player_id]);
    }
    if current {
        if !state.table.is_empty() {
            println!("  table: {}", fmt_dice(&state.table));
        }
        if state.turn_total > 0 {
            println!("  this turn (unbanked): {}", state.turn_total);
        }
    }
}

fn next_hint(state: &GameState) -> String {
    let name = &state.current_player().name;
    match state.phase {
        Phase::AwaitRoll => format!("{name}'s turn — roll: play <password> roll"),
        Phase::AwaitKeep => format!(
            "{name}: keep scoring dice from {}, e.g. play <password> keep <dice...>",
            fmt_dice(&state.table)
        ),
        Phase::AwaitRollOrBank => format!(
            "{name}: roll again ({} dice) or bank {} — play <password> roll | bank",
            state.dice_available, state.turn_total
        ),
        Phase::GameOver => "game over".into(),
    }
}

fn fmt_dice(dice: &[u8]) -> String {
    dice.iter()
        .map(|d| d.to_string())
        .collect::<Vec<_>>()
        .join(" ")
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
