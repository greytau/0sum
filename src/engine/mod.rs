//! The game-agnostic engine (PHASE2 §9): the `Game` trait, replay-as-verify,
//! signed append, and `apply_chain` (issue #8's seam). No I/O lives here.

pub mod keys;
pub mod log;

use std::fmt::{self, Debug};

use ed25519_dalek::{SigningKey, VerifyingKey};
use rand::rngs::StdRng;
use serde::de::DeserializeOwned;
use serde::Serialize;

use keys::Secrets;
use log::{now, Body, Entry, Genesis, Hex, Hex32, Register, Seat, Session, VERSION};

pub struct SessionInfo {
    #[expect(
        dead_code,
        reason = "Liar's Dice binds commitments to the SID (PHASE2 §4.1)"
    )]
    pub session_id: String,
    pub seats: Vec<String>,
}

pub trait Game: 'static {
    const ID: &'static str;
    const DEFAULT_RULESET: &'static str;
    type Rules: Serialize + DeserializeOwned + Clone;
    type State: Clone;
    type Move: Clone + Debug + PartialEq;
    /// Must be `#[serde(deny_unknown_fields)]`: replay parses it strictly.
    type Event: Serialize + DeserializeOwned + Clone + Debug;

    fn ruleset(name: &str) -> Result<Self::Rules, String>;
    /// One line for new/register output.
    fn describe(rules: &Self::Rules) -> String;
    fn initial(rules: Self::Rules, info: &SessionInfo) -> Self::State;
    fn parse_moves(tokens: &[String]) -> Result<Vec<Self::Move>, String>;

    /// THE rules engine: validate `ev` by `author` against `state`, then apply
    /// it. Used identically by live play and by verify-replay. Err = illegal.
    fn fold(state: &mut Self::State, author: usize, ev: &Self::Event) -> Result<(), String>;

    /// Turn a player's intent into events (may consume ctx.rng / ctx.secrets).
    fn plan(
        state: &Self::State,
        me: usize,
        mv: &Self::Move,
        ctx: &mut Ctx,
    ) -> Result<Vec<Self::Event>, String>;

    /// Next protocol entry this seat owes right now.
    fn duty(_state: &Self::State, _me: usize, _ctx: &Ctx) -> Option<Self::Event> {
        None
    }

    /// `None` = nobody (protocol wait / game over).
    fn to_move(state: &Self::State) -> Option<usize>;
    fn winner(state: &Self::State) -> Option<usize>;

    /// Who or what the table is waiting on when `to_move` is `None` mid-game.
    fn waiting(_state: &Self::State, _names: &[String]) -> String {
        "the other players".into()
    }

    fn narrate(
        before: &Self::State,
        author: usize,
        ev: &Self::Event,
        after: &Self::State,
        names: &[String],
    ) -> Vec<String>;
    fn render(
        state: &Self::State,
        names: &[String],
        me: Option<usize>,
        ctx: Option<&Ctx>,
    ) -> String;
}

pub struct Ctx {
    pub secrets: Secrets,
    pub rng: StdRng,
}

pub struct Table<G: Game> {
    pub session_id: String,
    pub rules: G::Rules,
    pub names: Vec<String>,
    pub seats: Vec<String>,
    pub keys: Vec<Option<VerifyingKey>>,
    pub state: G::State,
    pub head: Hex32,
}

impl<G: Game> Table<G> {
    pub fn started(&self) -> bool {
        self.keys.iter().all(Option::is_some)
    }

    pub fn seat_of(&self, key: &VerifyingKey) -> Option<usize> {
        self.keys.iter().position(|k| k.as_ref() == Some(key))
    }

    pub fn unregistered(&self) -> Vec<&str> {
        self.keys
            .iter()
            .zip(&self.names)
            .filter(|(k, _)| k.is_none())
            .map(|(_, n)| n.as_str())
            .collect()
    }
}

#[derive(Debug, PartialEq)]
pub struct Broken {
    pub seq: u64,
    pub reason: String,
}

impl fmt::Display for Broken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "BROKEN at seq {}: {}", self.seq, self.reason)
    }
}

fn broken<T>(seq: u64, reason: impl Into<String>) -> Result<T, Broken> {
    Err(Broken {
        seq,
        reason: reason.into(),
    })
}

/// Seat names: at least two, non-empty, unique case-insensitively (register
/// matches names case-insensitively, so `Alice`/`alice` would deadlock a seat).
pub fn check_roster(names: &[String]) -> Result<(), String> {
    if names.len() < 2 {
        return Err("need at least 2 players".into());
    }
    for (i, n) in names.iter().enumerate() {
        if n.trim().is_empty() {
            return Err(format!("player {} has an empty name", i + 1));
        }
        if names[..i].iter().any(|m| m.eq_ignore_ascii_case(n)) {
            return Err(format!(
                "two players are named `{n}` (names must differ ignoring case)"
            ));
        }
    }
    Ok(())
}

/// A fresh session holding only its (unsigned) genesis.
pub fn new_session<G: Game>(
    sid: &str,
    rules: &G::Rules,
    names: &[String],
    nonce: [u8; 16],
) -> Result<Session, String> {
    check_roster(names)?;
    let genesis = Genesis {
        session_id: sid.to_string(),
        game: G::ID.to_string(),
        ruleset: serde_json::to_value(rules).map_err(|e| format!("serialize ruleset: {e}"))?,
        roster: names
            .iter()
            .enumerate()
            .map(|(i, n)| Seat {
                seat: format!("p{}", i + 1),
                name: n.clone(),
            })
            .collect(),
        nonce: Hex(nonce),
    };
    Ok(Session {
        version: VERSION,
        session_id: sid.to_string(),
        log: vec![Entry::genesis(now(), genesis)],
    })
}

/// The game id a session claims, read from its genesis.
pub fn genesis_game(s: &Session) -> Result<&str, Broken> {
    match s.log.first().map(|e| &e.body) {
        Some(Body::Genesis(g)) => Ok(&g.game),
        Some(_) => broken(0, "first entry isn't genesis"),
        None => broken(0, "empty log"),
    }
}

/// `head <seq>:<first 12 hex>` — the out-of-band rollback check (§12.2).
pub fn head_line(s: &Session) -> String {
    let last = s.log.last().expect("a loaded session has genesis");
    format!("head {}:{}", last.seq, &hex::encode(last.hash.0)[..12])
}

/// Full §3.4 replay. Every command runs this before acting.
pub fn replay<G: Game>(s: &Session) -> Result<Table<G>, Broken> {
    replay_inner(s, None)
}

/// Replay that also narrates every entry, for `verify`.
pub fn replay_narrated<G: Game>(s: &Session, out: &mut Vec<String>) -> Result<Table<G>, Broken> {
    replay_inner(s, Some(out))
}

fn check_envelope(i: u64, e: &Entry, prev: &Hex32) -> Result<(), Broken> {
    if e.seq != i {
        return broken(i, format!("seq is {}, expected {i}", e.seq));
    }
    if e.prev != *prev {
        return broken(i, "prev doesn't match the previous entry's hash");
    }
    if e.hash != e.recompute_hash() {
        return broken(i, "hash doesn't match the entry's contents");
    }
    Ok(())
}

fn replay_inner<G: Game>(
    s: &Session,
    mut out: Option<&mut Vec<String>>,
) -> Result<Table<G>, Broken> {
    if s.version != VERSION {
        return broken(0, format!("version {}, expected {VERSION}", s.version));
    }
    let first = s.log.first().ok_or(Broken {
        seq: 0,
        reason: "empty log".into(),
    })?;
    check_envelope(0, first, &Hex([0; 32]))?;
    let Body::Genesis(g) = &first.body else {
        return broken(0, "first entry isn't genesis");
    };
    if first.author.is_some() || first.sig.is_some() {
        return broken(0, "genesis must have no author and no signature");
    }
    if g.game != G::ID {
        return broken(0, format!("game is `{}`, not `{}`", g.game, G::ID));
    }
    if g.session_id != s.session_id {
        return broken(
            0,
            format!(
                "genesis session id {} doesn't match the file's {}",
                g.session_id, s.session_id
            ),
        );
    }
    let rules: G::Rules = serde_json::from_value(g.ruleset.clone())
        .or_else(|e| broken(0, format!("ruleset doesn't parse: {e}")))?;
    let names: Vec<String> = g.roster.iter().map(|r| r.name.clone()).collect();
    let seats: Vec<String> = g.roster.iter().map(|r| r.seat.clone()).collect();
    check_roster(&names).or_else(|e| broken(0, e))?;
    if seats
        .iter()
        .enumerate()
        .any(|(i, id)| *id != format!("p{}", i + 1))
    {
        return broken(0, "seat ids must be p1..pN in order");
    }
    let info = SessionInfo {
        session_id: s.session_id.clone(),
        seats: seats.clone(),
    };
    if let Some(out) = out.as_deref_mut() {
        let roster: Vec<String> = g
            .roster
            .iter()
            .map(|r| format!("{} {}", r.seat, r.name))
            .collect();
        out.push(format!(
            "{:>4}  genesis: {}; seats: {}",
            0,
            G::describe(&rules),
            roster.join(", ")
        ));
    }
    let mut t = Table::<G> {
        session_id: s.session_id.clone(),
        state: G::initial(rules.clone(), &info),
        rules,
        keys: vec![None; seats.len()],
        names,
        seats,
        head: first.hash,
    };

    for (i, e) in s.log.iter().enumerate().skip(1) {
        let i = i as u64;
        check_envelope(i, e, &t.head)?;
        let author = e
            .author
            .as_deref()
            .ok_or(Broken {
                seq: i,
                reason: "entry has no author".into(),
            })?
            .to_string();
        match &e.body {
            Body::Genesis(_) => return broken(i, "a second genesis"),
            Body::Register(r) => {
                if r.seat != author {
                    return broken(i, "a seat can only register itself");
                }
                let seat = t.seats.iter().position(|id| *id == r.seat).ok_or(Broken {
                    seq: i,
                    reason: format!("no seat `{}`", r.seat),
                })?;
                if t.keys[seat].is_some() {
                    return broken(i, format!("seat {} registered twice", r.seat));
                }
                let key = VerifyingKey::from_bytes(&r.pubkey.0)
                    .or_else(|_| broken(i, "pubkey isn't a valid point"))?;
                if t.seat_of(&key).is_some() {
                    return broken(i, "pubkey already belongs to another seat");
                }
                e.verify_sig(&key).or_else(|r| broken(i, r))?;
                t.keys[seat] = Some(key);
                if let Some(out) = out.as_deref_mut() {
                    out.push(format!(
                        "{i:>4}  {} registered (key {})",
                        t.names[seat],
                        &hex::encode(r.pubkey.0)[..12]
                    ));
                }
            }
            Body::Game(v) => {
                if !t.started() {
                    return broken(i, "game move before every seat registered");
                }
                let seat = t.seats.iter().position(|id| *id == author).ok_or(Broken {
                    seq: i,
                    reason: format!("no seat `{author}`"),
                })?;
                let key = t.keys[seat].expect("started implies registered");
                e.verify_sig(&key).or_else(|r| broken(i, r))?;
                let ev: G::Event = serde_json::from_value(v.clone())
                    .or_else(|e| broken(i, format!("event doesn't parse: {e}")))?;
                let before = out.is_some().then(|| t.state.clone());
                G::fold(&mut t.state, seat, &ev).or_else(|r| broken(i, r))?;
                if let (Some(out), Some(before)) = (out.as_deref_mut(), before) {
                    for line in G::narrate(&before, seat, &ev, &t.state, &t.names) {
                        out.push(format!("{i:>4}  {line}"));
                    }
                }
            }
        }
        t.head = e.hash;
    }
    Ok(t)
}

/// Sign and append a `register` for `seat`.
pub fn append_register<G: Game>(
    s: &mut Session,
    t: &mut Table<G>,
    seat: usize,
    sk: &SigningKey,
) -> Result<(), String> {
    let key = sk.verifying_key();
    if t.keys[seat].is_some() {
        return Err(format!("`{}` has already registered", t.names[seat]));
    }
    if t.seat_of(&key).is_some() {
        return Err("that key already belongs to another seat".into());
    }
    let body = Body::Register(Register {
        seat: t.seats[seat].clone(),
        pubkey: Hex(key.to_bytes()),
    });
    let e = Entry::signed(s.log.len() as u64, now(), &t.seats[seat], t.head, body, sk);
    t.head = e.hash;
    s.log.push(e);
    t.keys[seat] = Some(key);
    Ok(())
}

/// Fold FIRST (on a copy, so an illegal event leaves no trace), then push the
/// signed entry.
pub fn append<G: Game>(
    s: &mut Session,
    t: &mut Table<G>,
    author: usize,
    ev: G::Event,
    sk: &SigningKey,
) -> Result<(), String> {
    debug_assert_eq!(t.keys[author], Some(sk.verifying_key()));
    let mut next = t.state.clone();
    G::fold(&mut next, author, &ev)?;
    let body = Body::Game(serde_json::to_value(&ev).map_err(|e| format!("serialize event: {e}"))?);
    let e = Entry::signed(
        s.log.len() as u64,
        now(),
        &t.seats[author],
        t.head,
        body,
        sk,
    );
    t.head = e.hash;
    s.log.push(e);
    t.state = next;
    Ok(())
}

pub struct Step<G: Game> {
    pub author: usize,
    pub event: G::Event,
    pub before: G::State,
    pub after: G::State,
}

pub struct Chain<G: Game> {
    pub steps: Vec<Step<G>>,
    pub failure: Option<String>,
}

fn push_step<G: Game>(
    s: &mut Session,
    t: &mut Table<G>,
    me: usize,
    ev: G::Event,
    sk: &SigningKey,
    steps: &mut Vec<Step<G>>,
) -> Result<(), String> {
    let before = t.state.clone();
    append(s, t, me, ev.clone(), sk)?;
    steps.push(Step {
        author: me,
        event: ev,
        before,
        after: t.state.clone(),
    });
    Ok(())
}

fn run_duties<G: Game>(
    s: &mut Session,
    t: &mut Table<G>,
    me: usize,
    ctx: &Ctx,
    sk: &SigningKey,
    steps: &mut Vec<Step<G>>,
) -> Result<(), String> {
    while let Some(ev) = G::duty(&t.state, me, ctx) {
        push_step(s, t, me, ev, sk, steps)?;
    }
    Ok(())
}

/// Issue #8's seam, generalized. No I/O, no printing, no entropy of its own.
/// The legal prefix persists; a turn-ending move halts the chain via the
/// `to_move` check and a win via the `winner` check.
pub fn apply_chain<G: Game>(
    s: &mut Session,
    t: &mut Table<G>,
    me: usize,
    moves: &[G::Move],
    ctx: &mut Ctx,
    sk: &SigningKey,
) -> Chain<G> {
    let mut steps = Vec::new();
    let failure = (|| {
        run_duties(s, t, me, ctx, sk, &mut steps)?;
        for mv in moves {
            if G::winner(&t.state).is_some() {
                return Err("the game is over".to_string());
            }
            match G::to_move(&t.state) {
                Some(p) if p == me => {}
                Some(other) => return Err(format!("it's {}'s turn, not yours", t.names[other])),
                None => return Err(format!("waiting on {}", G::waiting(&t.state, &t.names))),
            }
            for ev in G::plan(&t.state, me, mv, ctx)? {
                push_step(s, t, me, ev, sk, &mut steps)?;
            }
            run_duties(s, t, me, ctx, sk, &mut steps)?;
        }
        Ok(())
    })()
    .err();
    Chain { steps, failure }
}

#[cfg(test)]
pub(crate) mod testkit {
    use super::*;
    use keys::Password;
    use rand::SeedableRng;

    pub const PASSWORDS: [&str; 3] = [
        "TEST01-AAAAAAAAAAAAAAAAAAAAAAAAAA",
        "TEST01-BBBBBBBBBBBBBBBBBBBBBBBBBB",
        "TEST01-CCCCCCCCCCCCCCCCCCCCCCCCCC",
    ];

    pub struct Player {
        pub ctx: Ctx,
        pub sk: SigningKey,
    }

    pub fn player(seat: usize) -> Player {
        let secrets = Secrets::from_password(&Password::parse(PASSWORDS[seat]).unwrap());
        Player {
            sk: secrets.signing_key(),
            ctx: Ctx {
                secrets,
                rng: StdRng::seed_from_u64(seat as u64),
            },
        }
    }

    /// An in-memory TEST01 session with every seat registered.
    pub fn seated<G: Game>(rules: &G::Rules, names: &[&str]) -> (Session, Table<G>, Vec<Player>) {
        let names: Vec<String> = names.iter().map(|n| n.to_string()).collect();
        let mut s = new_session::<G>("TEST01", rules, &names, [0; 16]).unwrap();
        let mut t = replay::<G>(&s).unwrap();
        let players: Vec<Player> = (0..names.len()).map(player).collect();
        for (seat, p) in players.iter().enumerate() {
            append_register(&mut s, &mut t, seat, &p.sk).unwrap();
        }
        (s, t, players)
    }
}

#[cfg(test)]
mod tests {
    use super::testkit::{player, seated};
    use super::*;
    use crate::farkle::ruleset::RuleSet;
    use crate::farkle::tests::{best_keep, roll_with, seed_where};
    use crate::farkle::{Event, Farkle, Move};
    use rand::SeedableRng;
    use serde_json::json;

    /// genesis, 2 registers, Alice: rolled, kept, banked; Bob: rolled.
    fn played() -> Session {
        let rules = RuleSet::kcd();
        let seed = seed_where(&rules, |scores| scores);
        let (mut s, mut t, mut players) = seated::<Farkle>(&rules, &["Alice", "Bob"]);
        players[0].ctx.rng = StdRng::seed_from_u64(seed);
        let keep = best_keep(&roll_with(seed, 6), &rules);
        let p = &mut players[0];
        let c = apply_chain(
            &mut s,
            &mut t,
            0,
            &[Move::Roll, Move::Keep(keep), Move::Bank],
            &mut p.ctx,
            &p.sk,
        );
        assert_eq!(c.failure, None);
        let p = &mut players[1];
        let c = apply_chain(&mut s, &mut t, 1, &[Move::Roll], &mut p.ctx, &p.sk);
        assert_eq!(c.failure, None);
        assert_eq!(s.log.len(), 7);
        s
    }

    fn rehash(e: &mut Entry) {
        e.hash = e.recompute_hash();
    }

    fn resign(e: &mut Entry, seat: usize) {
        let author = e.author.clone().unwrap();
        *e = Entry::signed(
            e.seq,
            e.ts,
            &author,
            e.prev,
            e.body.clone(),
            &player(seat).sk,
        );
    }

    fn broken_at(s: &Session) -> Broken {
        replay::<Farkle>(s).err().expect("replay must fail")
    }

    #[test]
    fn untouched_log_verifies() {
        let s = played();
        let mut narr = Vec::new();
        let t = replay_narrated::<Farkle>(&s, &mut narr).unwrap();
        assert_eq!(t.head, s.log[6].hash);
        assert!(
            narr[0].contains("genesis: Farkle, ruleset kcd (target 4000); seats: p1 Alice, p2 Bob")
        );
        assert!(narr[3].contains("Alice rolled: "));
    }

    #[test]
    fn tamper_edit_is_a_hash_mismatch() {
        let mut s = played();
        s.log[3].body = Body::Game(json!({ "rolled": { "dice": [1, 1, 1, 1, 1, 1] } }));
        assert_eq!(
            broken_at(&s).to_string(),
            "BROKEN at seq 3: hash doesn't match the entry's contents"
        );
    }

    #[test]
    fn tamper_edit_with_rehash_fails_the_signature() {
        let mut s = played();
        s.log[3].body = Body::Game(json!({ "rolled": { "dice": [1, 1, 1, 1, 1, 1] } }));
        rehash(&mut s.log[3]);
        assert_eq!(
            broken_at(&s).to_string(),
            "BROKEN at seq 3: signature doesn't verify under the author's key"
        );
    }

    #[test]
    fn tamper_reattribution_fails_the_signature() {
        let mut s = played();
        s.log[6].author = Some("p1".into());
        rehash(&mut s.log[6]);
        assert_eq!(
            broken_at(&s).to_string(),
            "BROKEN at seq 6: signature doesn't verify under the author's key"
        );
    }

    #[test]
    fn tamper_deleting_a_middle_entry_breaks_the_chain() {
        let mut s = played();
        s.log.remove(4);
        assert_eq!(broken_at(&s).seq, 4);
        // Even renumbered and rehashed, the prev link gives it away.
        for (i, e) in s.log.iter_mut().enumerate().skip(4) {
            e.seq = i as u64;
            rehash(e);
        }
        assert_eq!(
            broken_at(&s).to_string(),
            "BROKEN at seq 4: prev doesn't match the previous entry's hash"
        );
    }

    #[test]
    fn tamper_unknown_field_fails_to_parse() {
        let mut v = serde_json::to_value(played()).unwrap();
        v["log"][3]["extra"] = json!(true);
        assert!(serde_json::from_value::<Session>(v.clone()).is_err());
        v["log"][3].as_object_mut().unwrap().remove("extra");
        v["mood"] = json!("sneaky");
        assert!(serde_json::from_value::<Session>(v).is_err());
    }

    #[test]
    fn tamper_unknown_field_inside_a_signed_event_fails_to_parse() {
        let mut s = played();
        s.log[5].body = Body::Game(json!({ "banked": { "points": 9000 } }));
        resign(&mut s.log[5], 0);
        let b = broken_at(&s);
        assert_eq!(b.seq, 5);
        assert!(b.reason.starts_with("event doesn't parse"), "{b}");
    }

    #[test]
    fn tamper_genesis_after_register_breaks_seq_1() {
        let mut s = played();
        let Body::Genesis(g) = &mut s.log[0].body else {
            unreachable!()
        };
        g.ruleset["target_score"] = json!(100);
        assert_eq!(broken_at(&s).seq, 0);
        rehash(&mut s.log[0]);
        assert_eq!(
            broken_at(&s).to_string(),
            "BROKEN at seq 1: prev doesn't match the previous entry's hash"
        );
    }

    /// G6: a valid signature on an illegal move is still BROKEN.
    #[test]
    fn signed_illegal_move_is_rejected_by_fold() {
        let mut s = played();
        let mut st = replay::<Farkle>(&s).unwrap().state;
        let expected = Farkle::fold(&mut st, 0, &Event::Banked {}).unwrap_err();
        let body = Body::Game(serde_json::to_value(Event::Banked {}).unwrap());
        s.log.push(Entry::signed(
            7,
            0,
            "p1",
            s.log[6].hash,
            body,
            &player(0).sk,
        ));
        assert_eq!(
            broken_at(&s),
            Broken {
                seq: 7,
                reason: expected
            }
        );
    }

    fn register_p2_with(s: &mut Session, pubkey_of: usize, signer: usize) {
        s.log.truncate(2);
        let body = Body::Register(Register {
            seat: "p2".into(),
            pubkey: Hex(player(pubkey_of).sk.verifying_key().to_bytes()),
        });
        let prev = s.log[1].hash;
        s.log
            .push(Entry::signed(2, 0, "p2", prev, body, &player(signer).sk));
    }

    #[test]
    fn registering_needs_proof_of_possession() {
        let mut s = played();
        register_p2_with(&mut s, 2, 1);
        assert_eq!(
            broken_at(&s).to_string(),
            "BROKEN at seq 2: signature doesn't verify under the author's key"
        );
    }

    #[test]
    fn a_pubkey_cannot_serve_two_seats() {
        let mut s = played();
        register_p2_with(&mut s, 0, 0);
        assert_eq!(
            broken_at(&s).to_string(),
            "BROKEN at seq 2: pubkey already belongs to another seat"
        );
    }

    #[test]
    fn moves_before_everyone_registers_are_broken() {
        let mut s = played();
        s.log.remove(2);
        s.log.truncate(3);
        s.log[2].seq = 2;
        s.log[2].prev = s.log[1].hash;
        resign(&mut s.log[2], 0);
        assert_eq!(
            broken_at(&s).to_string(),
            "BROKEN at seq 2: game move before every seat registered"
        );
    }

    #[test]
    fn roster_names_must_differ_ignoring_case() {
        let names = |v: &[&str]| v.iter().map(|n| n.to_string()).collect::<Vec<_>>();
        assert!(check_roster(&names(&["Alice", "Bob"])).is_ok());
        assert!(check_roster(&names(&["Alice", "alice"]))
            .unwrap_err()
            .contains("names must differ ignoring case"));
        assert!(check_roster(&names(&["Alice"])).is_err());
        assert!(check_roster(&names(&["Alice", " "])).is_err());
    }
}
