use super::*;
use crate::engine::log::{Body, Entry, Session};
use crate::engine::testkit::{player, seated, Player};
use crate::engine::{
    append, apply_chain, new_session, replay, replay_narrated, resolve_format, Chain, FormatPrefs,
    Table,
};

type Seated = (Session, Table<Battleship>, Vec<Player>);

const ALICE: &str = "place carrier A1 h battleship C1 h cruiser E1 h submarine G1 h destroyer I1 h";
const BOB: &str = "place carrier C3 v battleship J7 h cruiser A6 v submarine E8 v destroyer A1 h";
/// Bob's 17 squares, in the order Alice sinks them: destroyer, cruiser,
/// carrier, submarine, battleship.
const BOB_SHIPS: [u16; 17] = [
    0, 1, 5, 15, 25, 22, 32, 42, 52, 62, 47, 57, 67, 96, 97, 98, 99,
];
const ALICE_SHIPS: [u16; 17] = [
    0, 1, 2, 3, 4, 20, 21, 22, 23, 40, 41, 42, 60, 61, 62, 80, 81,
];

fn tokens(line: &str) -> Vec<String> {
    line.split_whitespace().map(String::from).collect()
}

fn moves(line: &str) -> Vec<Move> {
    Battleship::parse_moves(&tokens(line)).unwrap()
}

fn game(rules: RuleSet) -> Seated {
    seated::<Battleship>(&rules, &["Alice", "Bob"])
}

fn go(g: &mut Seated, seat: usize, line: &str) -> Chain<Battleship> {
    let mv = if line.is_empty() {
        Vec::new()
    } else {
        moves(line)
    };
    let (s, t, players) = g;
    let p = &mut players[seat];
    apply_chain(s, t, seat, &mv, &mut p.ctx, &p.sk)
}

fn ok(g: &mut Seated, seat: usize, line: &str) {
    let c = go(g, seat, line);
    assert_eq!(c.failure, None, "{line}");
}

fn fails(g: &mut Seated, seat: usize, line: &str) -> String {
    go(g, seat, line).failure.expect("the chain must fail")
}

/// Both fleets of B§7.3 locked; Alice to fire.
fn locked() -> Seated {
    let mut g = game(RuleSet::classic());
    ok(&mut g, 0, &format!("{ALICE} ready"));
    ok(&mut g, 1, &format!("{BOB} ready"));
    assert_eq!(g.1.state.phase, Phase::Battle);
    g
}

fn seed_of(seat: usize) -> [u8; 32] {
    board::seed(&player(seat).ctx.secrets)
}

fn cell_salt(seat: usize, i: u16) -> Hex32 {
    Hex(slots::salt(&seed_of(seat), CELL, i))
}

fn ship_salt(seat: usize, k: usize) -> Hex32 {
    Hex(slots::salt(&seed_of(seat), SHIP, k as u16))
}

fn at(row: u8, col: u8, dir: Dir) -> Placement {
    Placement { row, col, dir }
}

fn h(row: u8, col: u8) -> Placement {
    at(row, col, Dir::H)
}

fn v(row: u8, col: u8) -> Placement {
    at(row, col, Dir::V)
}

fn seat(i: usize) -> &'static str {
    ["p1", "p2"][i]
}

/// Fold first, then sign and push (the engine's own `append`).
fn put(g: &mut Seated, seat: usize, ev: Event) -> Result<(), String> {
    let (s, t, players) = g;
    append(s, t, seat, ev, &players[seat].sk)
}

/// A validly signed entry pushed without folding it.
fn forge(s: &mut Session, seat: usize, ev: &Event) {
    let prev = s.log.last().unwrap().hash;
    let seq = s.log.len() as u64;
    let body = Body::Game(serde_json::to_value(ev).unwrap());
    s.log.push(Entry::signed(
        seq,
        0,
        self::seat(seat),
        prev,
        body,
        &player(seat).sk,
    ));
}

fn broken(s: &Session) -> String {
    replay::<Battleship>(s)
        .err()
        .expect("replay must fail")
        .to_string()
}

fn narrative(s: &Session) -> Vec<String> {
    let mut out = Vec::new();
    replay_narrated::<Battleship>(s, &mut out).unwrap();
    out
}

/// An answer opened with the seat's true salt for whatever bit it claims.
fn answer_with(seat: usize, i: u16, hit: bool, sunk: Option<(usize, Placement)>) -> Event {
    Event::Answer {
        cell: i,
        hit,
        salt: cell_salt(seat, i),
        sunk: sunk.map(|(k, p)| Sunk {
            ship: k as u8,
            salt: ship_salt(seat, k),
            at: p,
        }),
    }
}

/// The honest binary's answer for `seat`.
fn duty_of(g: &Seated, seat: usize) -> Option<Event> {
    Battleship::duty(&g.1.state, seat, &g.2[seat].ctx)
}

/// Alternate shots from the two lists. The defender answers with
/// `cheat(state, defender)` when it returns Some, else its honest duty.
/// Stops when the battle ends, a list runs out, or nobody can answer.
fn duel(g: &mut Seated, shots: [&[u16]; 2], cheat: &mut dyn FnMut(&State, usize) -> Option<Event>) {
    let mut next = [0, 0];
    while g.1.state.phase == Phase::Battle {
        let st = &g.1.state;
        if st.pending.is_some() {
            let d = 1 - st.turn;
            let Some(ev) = cheat(st, d).or_else(|| duty_of(g, d)) else {
                return;
            };
            put(g, d, ev).unwrap();
        } else {
            let a = st.turn;
            let Some(&cell) = shots[a].get(next[a]) else {
                return;
            };
            next[a] += 1;
            put(g, a, Event::Shot { cell }).unwrap();
        }
    }
}

fn honest(_: &State, _: usize) -> Option<Event> {
    None
}

fn water_of(ships: &[u16]) -> Vec<u16> {
    (0..100).filter(|i| !ships.contains(i)).collect()
}

fn outcome(g: &Seated) -> Outcome {
    g.1.state.outcome.expect("the game is over")
}

fn forfeit(g: &Seated) -> (usize, usize, Cheat) {
    match outcome(g) {
        Outcome {
            winner,
            how: How::Forfeit { liar, cheat },
        } => (winner, liar, cheat),
        o => panic!("expected a forfeit, got {o:?}"),
    }
}

/// A fleet commitment built by hand: placements and occupancy chosen by the
/// cheater, so `plan` and its checks never see them.
fn rig(g: &mut Seated, who: usize, fleet: &[Placement], occ: &[u8]) {
    let b = seed_of(who);
    let sid = g.1.state.sid.clone();
    let scope = board::ship_scope(&sid, seat(who));
    for (k, p) in fleet.iter().enumerate() {
        let c = Hex(board::ship_commitment(&scope, &b, k, *p));
        put(g, who, Event::Placed { ship: k as u8, c }).unwrap();
    }
    let scope = board::cell_scope(&sid, seat(who));
    let values: Vec<&[u8]> = occ.chunks(1).collect();
    let cells = slots::commit_all(&scope, &b, &values);
    put(
        g,
        who,
        Event::Fleet {
            cells: cells.into_iter().map(Hex).collect(),
        },
    )
    .unwrap();
}

fn occ_of(squares: &[u16]) -> Vec<u8> {
    (0..100).map(|i| squares.contains(&i) as u8).collect()
}

fn alice_fleet() -> Vec<Placement> {
    vec![h(0, 0), h(2, 0), h(4, 0), h(6, 0), h(8, 0)]
}

fn bob_fleet() -> Vec<Placement> {
    vec![v(2, 2), h(9, 6), v(0, 5), v(4, 7), h(0, 0)]
}

// ---------------------------------------------------------------- vectors

fn hexs(b: &[u8]) -> String {
    hex::encode(b)
}

fn digest(parts: &[[u8; 32]]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    for p in parts {
        h.update(p);
    }
    hex::encode(h.finalize())
}

fn committed(g: &Seated, who: usize) -> (Vec<[u8; 32]>, Vec<[u8; 32]>) {
    let b = &g.1.state.boards[who];
    (
        b.placed.iter().map(|c| c.unwrap()).collect(),
        b.cells.clone().unwrap(),
    )
}

/// B§14, against tests/vectors/bs_vectors.py's output.
#[test]
fn golden_vectors_match_the_python_reference() {
    let g = locked();
    assert_eq!(
        hexs(&seed_of(0)),
        "f2cf63d2c7bbaf44417ba15b2f8cdffa34a0f9e69f21bc68415e2f1a246f21f3"
    );
    assert_eq!(
        hexs(&seed_of(1)),
        "4f471cbb4e6e65d9a9f67a08a6bc12b9624041c2981238fd84aa9e67854321e6"
    );
    let (s, k) = committed(&g, 0);
    let want_s = [
        "4db202c95b85b97645ac7961b547635db0f5933210dc45cfd50ea69c2b433702",
        "f79dee13e5f6e640e64e2cf442661a85c2174448e2f02f48b65f90d3befcc182",
        "65d543f3b4dfe42e718f680701b70c71cbdc9caaf07111ca68d87bc036e852d4",
        "a6d8a42fe3a7c68946108b4772f45bb99379af57ce91b3d21358981badbb0285",
        "21ce6757bca95a2fc63f1f78d0f97eb7e03ec3fc49401b45d12ed97aafd292a5",
    ];
    for (c, w) in s.iter().zip(want_s) {
        assert_eq!(hexs(c), w);
    }
    assert_eq!(
        hexs(&k[0]),
        "4ca980dbe7ea55aadfafa83e3f82f5a03a6f158722680f12965600f90b9119ba"
    );
    assert_eq!(
        hexs(&k[1]),
        "452b90ccb3b925d8a313e74743301f8e5ed5b18d195eed83ad471cefbfdc0862"
    );
    assert_eq!(
        hexs(&cell_salt(0, 44).0),
        "1703a822b626e86667ad36986b87e639ee26c88049c4e9098d963c70c5c3629f"
    );
    assert_eq!(
        hexs(&k[44]),
        "f218241ed851caea56d6c0d6f89eb389de30232968c0063f6c3e14a8688d3b99"
    );
    assert_eq!(
        digest(&k),
        "058947ed5273739c5ccb405484c89c03ca1c977b2ae730c9e1711a77693c6545"
    );
    assert_eq!(
        digest(&s),
        "804d59ce645237414ca8821a3f0acd9f9523864c95da8ce9f2ed396defc79988"
    );

    let (s, k) = committed(&g, 1);
    let want_s = [
        "ed6e74c4ca707440e2661f1c2a78eebf90af71b9f5833e2c3d075fa3e7897e68",
        "b12eaa5eb4018aaaa5009652cc80ee1f1e5a716825f811ee1a54f925192780bd",
        "e0e600130abc9f1d927711d462d72580c41aa1988a94cc187622e3aea1ea7968",
        "05205549d2c91bfe650be4260a894f4804090e7b11669b6c61d0953afdde58ec",
        "3146c13268d6418abbf8eba365e801de01198d5cb3e1001c9acc22531d94beff",
    ];
    for (c, w) in s.iter().zip(want_s) {
        assert_eq!(hexs(c), w);
    }
    assert_eq!(
        hexs(&ship_salt(1, 4).0),
        "0b1a8baadc966c59769fd21f00db22bed833e3ca632f7b6c7b586c81870fc7b6"
    );
    for (i, salt, c) in [
        (
            0,
            "2611a88ad33250687eb28449f01ba06db6c72ad7d39515b6ce6eb0c8c3c464b9",
            "335ec1682b9edcd101116472eadcddf0111aaa0fbeef74423a4085901a853861",
        ),
        (
            1,
            "c07fedc4bb6638afbb0a2fbc177f4f72819718e9a68c75a0f2d348ed64840d05",
            "fa676b548043c3f3590398929a2cd2684e8d3645b23c1f36c11a90b5dacea312",
        ),
        (
            44,
            "a392a1d935272d9a8a64a3683acc3058cf02155d38157239214f7acca049b1ee",
            "5e0cc762201fe894db0f018573b17feca68c2b98ad4c4db05e67da4748cc9d7a",
        ),
    ] {
        assert_eq!(hexs(&cell_salt(1, i).0), salt);
        assert_eq!(hexs(&k[i as usize]), c);
    }
    assert_eq!(
        digest(&k),
        "f30627da49421fb9072b5a2cf2c3bf9028be7c14500fc861d59acef811c3811c"
    );
    assert_eq!(
        digest(&s),
        "b880ef5f33a84f33f649a358bc3c49d748b6a5cf638460838c42c005051d5d27"
    );

    // The cheat vectors: none equals the honest commitment it targets.
    let s0 = cell_salt(1, 0).0;
    let cell = |sid, seat, v| slots::commit(&board::cell_scope(sid, seat), 0, &s0, &[v]);
    let lie = cell("TEST01", "p2", 0);
    assert_eq!(
        hexs(&lie),
        "7b0b730ff794c977d8af8498b7f70fdcf937248e0f9b7f81e8a26b5f951de65e"
    );
    let moved = board::ship_commitment(&board::ship_scope("TEST01", "p2"), &seed_of(1), 4, v(0, 0));
    assert_eq!(
        hexs(&moved),
        "668220fe6df11dfaa6a250745b1d7c2d7f3c91051c0a5b3aec97202fc87360f6"
    );
    let other_seat = cell("TEST01", "p1", 1);
    assert_eq!(
        hexs(&other_seat),
        "ee2116377bce3465b6286fc140d7677e6756ffdff35ed7a0b1e59c6f7d9447f3"
    );
    let other_sid = cell("TEST02", "p2", 1);
    assert_eq!(
        hexs(&other_sid),
        "8e114d210e57b7a8fa788ed1dec2d36c005181aed05fa9f69ec07e991be8c8bf"
    );
    for c in [lie, other_seat, other_sid] {
        assert_ne!(c, k[0]);
    }
    assert_ne!(moved, s[4]);
}

// ---------------------------------------------------------------- read-back

#[test]
fn read_back_recovers_placements_only_with_the_owners_password() {
    let mut g = game(RuleSet::classic());
    ok(&mut g, 0, "place carrier A1 h destroyer J9 h carrier B2 v");
    let st = &g.1.state;
    let mine = read_own(st, 0, &seed_of(0)).unwrap();
    assert_eq!(mine, vec![Some(v(1, 1)), None, None, None, Some(h(9, 8))]);
    assert_eq!(
        read_own(st, 0, &seed_of(1)).unwrap_err(),
        "can't read your carrier back from the log with this password"
    );
    let replayed = replay::<Battleship>(&g.0).unwrap();
    assert_eq!(replayed.state, g.1.state);
}

// ---------------------------------------------------------------- a full game

/// B§7.3 through `apply_chain`: every answer is a duty, never written by hand.
#[test]
fn a_full_game_reaches_over_and_verifies() {
    let mut g = game(RuleSet::classic());
    let c = go(&mut g, 0, &format!("{ALICE} ready"));
    assert_eq!(c.failure, None);
    assert_eq!(c.steps.len(), 6);
    assert_eq!(
        go(&mut g, 0, "place carrier A1 h").failure.as_deref(),
        Some("your fleet is locked; waiting on Bob to place theirs")
    );
    ok(&mut g, 1, &format!("{BOB} ready"));
    assert_eq!(Battleship::to_move(&g.1.state), Some(0));
    assert_eq!(g.0.log.len(), 15);

    let water = water_of(&ALICE_SHIPS);
    let mut bob_shots = vec![44u16];
    bob_shots.extend(water.iter().filter(|&&i| i != 44).take(16));
    for n in 0..17 {
        let sq = board::sq_name(10, BOB_SHIPS[n]);
        let c = go(&mut g, 0, &sq);
        assert_eq!(c.failure, None, "Alice's shot {n} at {sq}");
        if n == 16 {
            break;
        }
        let sq = board::sq_name(10, bob_shots[n]);
        let c = go(&mut g, 1, &sq);
        assert_eq!(c.failure, None, "Bob's shot {n} at {sq}");
    }
    let c = go(&mut g, 1, "");
    assert_eq!(
        c.steps.len(),
        1,
        "Bob's last answer, and no reveal from Bob"
    );
    assert_eq!(g.1.state.phase, Phase::Reveal);
    assert_eq!(g.1.state.provisional, Some(0));
    assert_eq!(Battleship::winner(&g.1.state), None);
    assert_eq!(
        Battleship::waiting(&g.1.state, &g.1.names),
        "Alice's final reveal"
    );
    let c = go(&mut g, 0, "");
    assert_eq!(c.steps.len(), 1);
    assert_eq!(
        outcome(&g),
        Outcome {
            winner: 0,
            how: How::Sank
        }
    );
    assert_eq!(
        Battleship::game_over(&g.1.state, &g.1.names).unwrap(),
        "Alice won: sank Bob's fleet; Alice's fleet verified"
    );

    let (s, t, _) = &g;
    let kinds: Vec<String> = s.log[15..21]
        .iter()
        .map(|e| {
            let Body::Game(v) = &e.body else {
                unreachable!()
            };
            format!(
                "{}:{}",
                e.author.as_deref().unwrap(),
                v.as_object().unwrap().keys().next().unwrap()
            )
        })
        .collect();
    assert_eq!(
        kinds,
        [
            "p1:shot",
            "p2:answer",
            "p2:shot",
            "p1:answer",
            "p1:shot",
            "p2:answer"
        ]
    );
    let Body::Game(sunk) = &s.log[20].body else {
        unreachable!()
    };
    assert_eq!(
        sunk["answer"]["sunk"],
        serde_json::json!({"ship": 4, "salt": hexs(&ship_salt(1, 4).0), "at": {"row": 0, "col": 0, "dir": "h"}})
    );
    let replayed = replay::<Battleship>(s).unwrap();
    assert_eq!(replayed.state, t.state);
    assert_eq!(replayed.last_move, t.last_move);
    assert_eq!(
        t.recap_line(0),
        format!(
            "your last move: fired at J10 (shot 33, seq {})",
            s.log.len() - 3
        )
    );

    let text = narrative(s).join("\n");
    for line in [
        "   3  Alice placed the carrier",
        "   8  Alice locked the fleet",
        "  14  Both fleets locked. Alice fires first.",
        "  15  Alice fires at A1",
        "  16  Bob: A1 — hit",
        "  18  Alice: E5 — miss",
        "  20  Bob: A2 — hit, Alice sank Bob's destroyer",
        "Alice sank Bob's fleet; waiting on Alice's final reveal to confirm",
        "Alice reveals: carrier A1 h, battleship C1 h, cruiser E1 h, submarine G1 h, destroyer I1 h — legal, every answer matched",
        "Alice won: sank Bob's fleet; Alice's fleet verified",
    ] {
        assert!(text.contains(line), "missing `{line}` in\n{text}");
    }
}

// ---------------------------------------------------------------- placement

#[test]
fn placement_errors_are_worded_for_humans() {
    let mut g = game(RuleSet::classic());
    for (line, want) in [
        (
            "place carrier H8 h",
            "the carrier doesn't fit at H8 across (5 long; the board ends at column 10)",
        ),
        (
            "place carrier H1 v",
            "the carrier doesn't fit at H1 down (5 long; the board ends at row J)",
        ),
        (
            "place carrier K1 h",
            "`K1` isn't on the board (rows A-J, columns 1-10)",
        ),
        (
            "place c A1 h",
            "`c` could be carrier or cruiser — type more letters",
        ),
        (
            "place x A1 h",
            "unknown ship `x` (carrier, battleship, cruiser, submarine, destroyer)",
        ),
        (
            "ready",
            "place the carrier, battleship, cruiser, submarine and destroyer first (or place auto)",
        ),
    ] {
        assert_eq!(fails(&mut g, 0, line), want, "{line}");
    }
    ok(&mut g, 0, "place b A3 v");
    assert_eq!(
        fails(&mut g, 0, "place carrier A1 h"),
        "the carrier at A1 h overlaps your battleship at A3"
    );
    ok(&mut g, 0, "place b F3 v ca A1 h cr C5 h d J1 h");
    assert_eq!(
        fails(&mut g, 0, "ready"),
        "place the submarine first (or place auto)"
    );
    ok(&mut g, 0, "place s E5 v");
    // Re-placing before ready is free: the cruiser moves, the board follows.
    ok(&mut g, 0, "place cruiser H5 h ready");
    let st = &g.1.state;
    let back = read_own(st, 0, &seed_of(0)).unwrap();
    assert_eq!(back[2], Some(h(7, 4)));
    assert!(st.locked(0));
    let occ = board::occupancy(
        &st.rules,
        &back.iter().flatten().copied().collect::<Vec<_>>(),
    );
    assert_eq!(occ[24], 0, "the cruiser's old square is water");
    assert_eq!(occ[74], 1);
}

#[test]
fn notouch_keeps_ships_apart_even_diagonally() {
    let mut g = game(RuleSet::notouch());
    ok(&mut g, 0, "place carrier A1 h");
    assert_eq!(
        fails(&mut g, 0, "place cruiser B1 h"),
        "the cruiser at B1 h touches your carrier (ruleset notouch keeps ships apart)"
    );
    assert_eq!(
        fails(&mut g, 0, "place cruiser B6 h"),
        "the cruiser at B6 h touches your carrier (ruleset notouch keeps ships apart)"
    );
    ok(&mut g, 0, "place cruiser C1 h");
    let mut c = game(RuleSet::classic());
    ok(&mut c, 0, "place carrier A1 h cruiser B1 h");
}

#[test]
fn place_auto_fills_legally_and_rerolls_a_full_fleet() {
    for rules in [RuleSet::classic(), RuleSet::notouch()] {
        let mut g = game(rules.clone());
        ok(&mut g, 0, "place battleship E5 v");
        let c = go(&mut g, 0, "place auto");
        assert_eq!((c.failure, c.steps.len()), (None, 4));
        let first: Vec<Placement> = read_own(&g.1.state, 0, &seed_of(0))
            .unwrap()
            .into_iter()
            .flatten()
            .collect();
        assert_eq!(first[1], v(4, 4));
        assert_eq!(
            board::legality(&rules, &first, &board::occupancy(&rules, &first)),
            Ok(())
        );
        let c = go(&mut g, 0, "place auto ready");
        assert_eq!((c.failure, c.steps.len()), (None, 6));
        let again: Vec<Placement> = read_own(&g.1.state, 0, &seed_of(0))
            .unwrap()
            .into_iter()
            .flatten()
            .collect();
        assert_ne!(again, first);
        assert_eq!(
            board::legality(&rules, &again, &board::occupancy(&rules, &again)),
            Ok(())
        );
    }
}

#[test]
fn moves_parse_and_chain() {
    let p = |line: &str| Battleship::parse_moves(&tokens(line));
    let place = |ship: &str, square: &str, dir| Move::Place {
        ship: ship.into(),
        square: square.into(),
        dir,
    };
    assert_eq!(
        p("place carrier A1 h b c3 down ready b5").unwrap(),
        vec![
            place("carrier", "A1", Dir::H),
            place("b", "c3", Dir::V),
            Move::Ready,
            Move::Fire("b5".into())
        ]
    );
    assert_eq!(
        p("place auto lock f A1 shoot J10 B2").unwrap(),
        vec![
            Move::PlaceAuto,
            Move::Ready,
            Move::Fire("A1".into()),
            Move::Fire("J10".into()),
            Move::Fire("B2".into())
        ]
    );
    let usage = "place takes <ship> <square> <h|v>, e.g. `place carrier A1 h`, or `place auto`";
    for bad in [
        "place",
        "place carrier A1",
        "place carrier A1 x",
        "place ready",
    ] {
        assert_eq!(p(bad).unwrap_err(), usage, "{bad}");
    }
    assert_eq!(
        p("fire").unwrap_err(),
        "fire takes a square, e.g. `fire B5`"
    );
    assert!(p("yeet").unwrap_err().starts_with("unknown move `yeet`"));
    assert!(p("").is_err());
}

// ---------------------------------------------------------------- shots

#[test]
fn shots_out_of_turn_twice_off_board_or_repeated_are_refused() {
    let mut g = game(RuleSet::classic());
    ok(&mut g, 1, &format!("{BOB} ready"));
    assert_eq!(fails(&mut g, 0, "fire B5"), "the battle hasn't started");
    assert_eq!(
        fails(&mut g, 1, "place carrier A1 h"),
        "your fleet is locked; waiting on Alice to place theirs"
    );
    ok(&mut g, 0, &format!("{ALICE} ready"));
    assert_eq!(fails(&mut g, 1, "B5"), "it's Alice's turn, not yours");
    assert_eq!(
        fails(&mut g, 0, "K1"),
        "`K1` isn't on the board (rows A-J, columns 1-10)"
    );
    let c = go(&mut g, 0, "fire B5 fire B6");
    assert_eq!(c.steps.len(), 1, "the legal prefix persists");
    assert_eq!(
        c.failure.as_deref(),
        Some("waiting on Bob's answer to the shot at B5")
    );
    ok(&mut g, 1, "D5");
    ok(&mut g, 0, "");
    assert_eq!(fails(&mut g, 0, "b5"), "you already fired at B5");

    // The same moves, signed and forged past `plan`: fold's own words.
    let base = g.0.clone();
    let mut s = base.clone();
    forge(&mut s, 1, &Event::Shot { cell: 3 });
    assert_eq!(broken(&s), "BROKEN at seq 19: p2 fired out of turn");
    let mut s = base.clone();
    forge(&mut s, 0, &Event::Shot { cell: 14 });
    assert_eq!(broken(&s), "BROKEN at seq 19: p1 already fired at B5");
    let mut s = base.clone();
    forge(&mut s, 0, &Event::Shot { cell: 100 });
    assert_eq!(
        broken(&s),
        "BROKEN at seq 19: square 100 isn't on the board"
    );
    let mut s = base.clone();
    forge(&mut s, 0, &Event::Shot { cell: 3 });
    forge(&mut s, 0, &Event::Shot { cell: 4 });
    assert_eq!(
        broken(&s),
        "BROKEN at seq 20: p1 fired again before p2 answered"
    );
    let mut s = base.clone();
    forge(&mut s, 1, &answer_with(1, 3, false, None));
    assert_eq!(
        broken(&s),
        "BROKEN at seq 19: no shot is waiting for an answer"
    );
    let mut s = base.clone();
    forge(&mut s, 0, &Event::Shot { cell: 3 });
    forge(&mut s, 0, &answer_with(0, 3, false, None));
    assert_eq!(
        broken(&s),
        "BROKEN at seq 20: only p2 can answer the shot at A4"
    );
    let mut s = base.clone();
    forge(&mut s, 0, &Event::Shot { cell: 3 });
    forge(&mut s, 1, &answer_with(1, 4, false, None));
    assert_eq!(
        broken(&s),
        "BROKEN at seq 20: p2 answered A5 but the shot was at A4"
    );
    let mut s = base;
    forge(
        &mut s,
        0,
        &Event::Reveal {
            b: Hex(seed_of(0)),
            fleet: alice_fleet(),
        },
    );
    assert_eq!(
        broken(&s),
        "BROKEN at seq 19: reveal before the battle is over"
    );
}

#[test]
fn setup_entries_are_checked_in_shape_and_order() {
    let g = game(RuleSet::classic());
    let base = g.0.clone();
    let c = Hex([1; 32]);
    let mut s = base.clone();
    forge(&mut s, 0, &Event::Placed { ship: 5, c });
    assert_eq!(broken(&s), "BROKEN at seq 3: no ship #5 in this fleet");
    let mut s = base.clone();
    forge(&mut s, 0, &Event::Placed { ship: 0, c });
    forge(
        &mut s,
        0,
        &Event::Fleet {
            cells: vec![c; 100],
        },
    );
    assert_eq!(
        broken(&s),
        "BROKEN at seq 4: p1 locked the fleet before placing the battleship"
    );
    let mut s = base.clone();
    for k in 0..5 {
        forge(&mut s, 0, &Event::Placed { ship: k, c });
    }
    forge(&mut s, 0, &Event::Fleet { cells: vec![c; 99] });
    assert_eq!(
        broken(&s),
        "BROKEN at seq 8: p1's fleet has 99 square commitments; the board has 100"
    );
    let mut s = base.clone();
    for k in 0..5 {
        forge(&mut s, 0, &Event::Placed { ship: k, c });
    }
    forge(
        &mut s,
        0,
        &Event::Fleet {
            cells: vec![c; 100],
        },
    );
    forge(
        &mut s,
        0,
        &Event::Fleet {
            cells: vec![c; 100],
        },
    );
    assert_eq!(broken(&s), "BROKEN at seq 9: p1 locked the fleet twice");
    let mut s = base;
    forge(&mut s, 1, &answer_with(1, 0, true, None));
    assert_eq!(broken(&s), "BROKEN at seq 3: the battle hasn't started");
}

// ---------------------------------------------------------------- two players

#[test]
fn battleship_is_exactly_two_players() {
    let three: Vec<String> = ["Alice", "Bob", "Carol"].map(String::from).to_vec();
    assert_eq!(
        new_session::<Battleship>("TEST01", &RuleSet::classic(), &three, [0; 16]).unwrap_err(),
        "need exactly 2 players"
    );
    let mut s = game(RuleSet::classic()).0;
    s.log.truncate(1);
    let Body::Genesis(gen) = &mut s.log[0].body else {
        unreachable!()
    };
    gen.roster.push(crate::engine::log::Seat {
        seat: "p3".into(),
        name: "Carol".into(),
    });
    s.log[0].hash = s.log[0].recompute_hash();
    assert_eq!(broken(&s), "BROKEN at seq 0: need exactly 2 players");
}

// ---------------------------------------------------------------- cheating

/// Move a ship after `fleet`: a late `placed`, a sunk at a moved spot, a reveal listing one.
#[test]
fn a_ship_cant_move_after_the_fleet_is_locked() {
    let g = locked();
    let mut s = g.0.clone();
    let c = Hex(g.1.state.boards[1].placed[0].unwrap());
    forge(&mut s, 1, &Event::Placed { ship: 0, c });
    assert_eq!(
        broken(&s),
        "BROKEN at seq 15: p2 moved a ship after locking the fleet"
    );

    let mut g = locked();
    duel(&mut g, [&[0], &[44]], &mut honest);
    put(&mut g, 0, Event::Shot { cell: 1 }).unwrap();
    let mut s = g.0.clone();
    forge(&mut s, 1, &answer_with(1, 1, true, Some((4, v(0, 0)))));
    assert_eq!(
        broken(&s),
        "BROKEN at seq 20: p2's sunk destroyer doesn't open p2's destroyer commitment"
    );

    let mut g = bob_wins_to_reveal();
    let mut moved = bob_fleet();
    moved[4] = v(0, 0);
    let mut s = g.0.clone();
    forge(
        &mut s,
        1,
        &Event::Reveal {
            b: Hex(seed_of(1)),
            fleet: moved,
        },
    );
    let n = s.log.len() - 1;
    assert_eq!(
        broken(&s),
        format!(
            "BROKEN at seq {n}: p2's revealed destroyer doesn't open p2's destroyer commitment"
        )
    );
    // Control: Bob's honest reveal of the same game passes.
    ok(&mut g, 1, "");
    assert_eq!(outcome(&g).how, How::Sank);
}

/// Alice sank nothing; Bob sank all 17 of Alice's squares. Bob's reveal is owed.
fn bob_wins_to_reveal() -> Seated {
    let mut g = locked();
    let water = water_of(&BOB_SHIPS);
    duel(&mut g, [&water, &ALICE_SHIPS], &mut honest);
    assert_eq!(g.1.state.phase, Phase::Reveal);
    assert_eq!(g.1.state.provisional, Some(1));
    g
}

#[test]
fn a_lie_about_hit_or_miss_doesnt_open() {
    let mut g = locked();
    put(&mut g, 0, Event::Shot { cell: 0 }).unwrap();
    let mut s = g.0.clone();
    forge(&mut s, 1, &answer_with(1, 0, false, None));
    assert_eq!(
        broken(&s),
        "BROKEN at seq 16: p2's answer for A1 doesn't open p2's commitment for that square"
    );
    // An honest binary refuses to append it in the first place.
    assert!(put(&mut g, 1, answer_with(1, 0, false, None)).is_err());
    assert_eq!(g.0.log.len(), 16);
}

/// Bob commits one carrier square (C3) as water at `ready`.
fn rigged_bob() -> Seated {
    let mut g = game(RuleSet::classic());
    ok(&mut g, 0, &format!("{ALICE} ready"));
    let mut squares = BOB_SHIPS.to_vec();
    squares.retain(|&i| i != 22);
    rig(&mut g, 1, &bob_fleet(), &occ_of(&squares));
    assert_eq!(g.1.state.phase, Phase::Battle);
    g
}

#[test]
fn a_board_rigged_at_commit_is_caught_by_counting() {
    let mut g = rigged_bob();
    let mut alice_shots = BOB_SHIPS.to_vec();
    alice_shots.extend(water_of(&BOB_SHIPS));
    let mut bob_shots = water_of(&ALICE_SHIPS);
    bob_shots.extend(&ALICE_SHIPS[1..]);
    duel(&mut g, [&alice_shots, &bob_shots], &mut honest);
    assert_eq!(g.1.state.boards[1].hits, 16, "Alice can't pass 16 hits");
    assert_eq!(forfeit(&g), (0, 1, Cheat::TooManyMisses { misses: 84 }));
    let text = narrative(&g.0).join("\n");
    assert!(
        text.contains("CHEAT: 84 misses on Bob's board: it can't hold a 17-square fleet. Bob forfeits; Alice wins."),
        "{text}"
    );
}

#[test]
fn a_rigged_board_that_wins_first_forfeits_at_its_reveal() {
    let mut g = rigged_bob();
    let water = water_of(&BOB_SHIPS);
    duel(&mut g, [&water, &ALICE_SHIPS], &mut honest);
    assert_eq!(g.1.state.provisional, Some(1));
    ok(&mut g, 1, "");
    assert_eq!(forfeit(&g), (0, 1, Cheat::Hidden { ship: 0, cell: 22 }));
    assert_eq!(
        Battleship::game_over(&g.1.state, &g.1.names).unwrap(),
        "Alice won: Bob forfeits — Bob's carrier covers C3 but the board marks it as water"
    );
}

#[test]
fn a_sunk_with_an_unhit_square_forfeits_sunk_early() {
    let mut g = locked();
    put(&mut g, 0, Event::Shot { cell: 0 }).unwrap();
    put(&mut g, 1, answer_with(1, 0, true, Some((4, h(0, 0))))).unwrap();
    assert_eq!(forfeit(&g), (0, 1, Cheat::SunkEarly { ship: 4, cell: 1 }));
    let text = narrative(&g.0).join("\n");
    assert!(text.contains("  16  Bob: A1 — hit\n  16  CHEAT: Bob declared the destroyer sunk but A2 on it was never hit. Bob forfeits; Alice wins."), "{text}");
}

/// A5: a sunk on a miss is a forfeit, not BROKEN.
#[test]
fn a_sunk_on_a_miss_forfeits() {
    // The miss isn't on the ship: the ship was complete earlier, so SunkLate.
    let mut g = locked();
    let mut withhold = |st: &State, d: usize| {
        (d == 1 && st.pending == Some(1)).then(|| answer_with(1, 1, true, None))
    };
    duel(&mut g, [&[0, 1], &[44, 45]], &mut withhold);
    put(&mut g, 0, Event::Shot { cell: 10 }).unwrap();
    put(&mut g, 1, answer_with(1, 10, false, Some((4, h(0, 0))))).unwrap();
    assert_eq!(forfeit(&g), (0, 1, Cheat::SunkLate { ship: 4, cell: 10 }));
    let text = narrative(&g.0).join("\n");
    assert!(text.contains("CHEAT: Bob declared the destroyer sunk on the shot at B1, which isn't on it: the sunk was held back. Bob forfeits; Alice wins."), "{text}");

    // The miss is on the ship (a rigged square): SunkEarly at that square.
    let mut g = game(RuleSet::classic());
    ok(&mut g, 0, &format!("{ALICE} ready"));
    let mut squares = BOB_SHIPS.to_vec();
    squares.retain(|&i| i != 1);
    rig(&mut g, 1, &bob_fleet(), &occ_of(&squares));
    duel(&mut g, [&[0], &[44]], &mut honest);
    put(&mut g, 0, Event::Shot { cell: 1 }).unwrap();
    put(&mut g, 1, answer_with(1, 1, false, Some((4, h(0, 0))))).unwrap();
    assert_eq!(forfeit(&g), (0, 1, Cheat::SunkEarly { ship: 4, cell: 1 }));
}

#[test]
fn a_sunk_held_back_for_good_forfeits_at_full_hits_or_at_reveal() {
    // Bob never declares the destroyer: 17 hits with 4 sunk.
    let mut g = locked();
    let mut strip = |st: &State, d: usize| {
        let ev = (d == 1)
            .then(|| Battleship::duty(st, 1, &player(1).ctx))
            .flatten()?;
        match ev {
            Event::Answer {
                cell,
                hit,
                salt,
                sunk: Some(s),
            } if s.ship == 4 => Some(Event::Answer {
                cell,
                hit,
                salt,
                sunk: None,
            }),
            _ => None,
        }
    };
    let water = water_of(&ALICE_SHIPS);
    duel(&mut g, [&BOB_SHIPS, &water], &mut strip);
    assert_eq!(forfeit(&g), (0, 1, Cheat::UnsunkAtFull { sunk: 4 }));
    assert!(narrative(&g.0).join("\n").contains(
        "CHEAT: 17 hits on Bob's board but only 4 ships declared sunk. Bob forfeits; Alice wins."
    ));

    // The winner withholds her own destroyer's sunk: caught at her reveal.
    let mut g = locked();
    let mut strip = |st: &State, d: usize| {
        let ev = (d == 0)
            .then(|| Battleship::duty(st, 0, &player(0).ctx))
            .flatten()?;
        match ev {
            Event::Answer {
                cell,
                hit,
                salt,
                sunk: Some(s),
            } if s.ship == 4 => Some(Event::Answer {
                cell,
                hit,
                salt,
                sunk: None,
            }),
            _ => None,
        }
    };
    let mut bob_shots = vec![80, 81];
    bob_shots.extend(water_of(&ALICE_SHIPS));
    duel(&mut g, [&BOB_SHIPS, &bob_shots], &mut strip);
    assert_eq!(g.1.state.provisional, Some(0));
    ok(&mut g, 0, "");
    assert_eq!(forfeit(&g), (1, 0, Cheat::Withheld { ship: 4 }));
    assert_eq!(
        Battleship::game_over(&g.1.state, &g.1.names).unwrap(),
        "Bob won: Alice forfeits — Alice's destroyer was hit on every square but never declared sunk"
    );
}

#[test]
fn a_ship_sunk_twice_is_broken() {
    let mut g = locked();
    duel(&mut g, [&[0, 1], &[44, 45]], &mut honest);
    assert_eq!(g.1.state.boards[1].sunk[4], Some(h(0, 0)));
    put(&mut g, 0, Event::Shot { cell: 10 }).unwrap();
    let mut s = g.0.clone();
    forge(&mut s, 1, &answer_with(1, 10, false, Some((4, h(0, 0)))));
    let n = s.log.len() - 1;
    assert_eq!(
        broken(&s),
        format!("BROKEN at seq {n}: p2's destroyer is already sunk")
    );
}

/// A1/A8 h: a carrier at A8 h (row 0, col 7) or on row 255 is OffBoard, no panic.
#[test]
fn a_bent_carrier_forfeits_off_board() {
    let bent: Vec<u16> = vec![7, 8, 9, 10, 11];
    let mut fleet = bob_fleet();
    fleet[0] = h(0, 7);
    let mut squares = BOB_SHIPS[..2].to_vec();
    squares.extend(&BOB_SHIPS[2..5]);
    squares.extend(&BOB_SHIPS[10..]);
    squares.extend(&bent);

    // The sunk.
    let mut g = game(RuleSet::classic());
    ok(&mut g, 0, &format!("{ALICE} ready"));
    rig(&mut g, 1, &fleet, &occ_of(&squares));
    let water = water_of(&ALICE_SHIPS);
    duel(&mut g, [&bent[..4], &water], &mut honest);
    assert_eq!(g.1.state.boards[1].hits, 4, "every bent square answers hit");
    put(&mut g, 0, Event::Shot { cell: 11 }).unwrap();
    put(&mut g, 1, answer_with(1, 11, true, Some((0, h(0, 7))))).unwrap();
    assert_eq!(
        forfeit(&g),
        (
            0,
            1,
            Cheat::OffBoard {
                ship: 0,
                at: h(0, 7)
            }
        )
    );
    assert!(narrative(&g.0)
        .join("\n")
        .contains("CHEAT: Bob's carrier runs off the board (A8 h). Bob forfeits; Alice wins."));

    // The sunk, with row 255.
    let mut g = game(RuleSet::classic());
    ok(&mut g, 0, &format!("{ALICE} ready"));
    let mut far = bob_fleet();
    far[0] = h(255, 0);
    rig(&mut g, 1, &far, &occ_of(&BOB_SHIPS));
    put(&mut g, 0, Event::Shot { cell: 0 }).unwrap();
    put(&mut g, 1, answer_with(1, 0, true, Some((0, h(255, 0))))).unwrap();
    assert_eq!(
        forfeit(&g),
        (
            0,
            1,
            Cheat::OffBoard {
                ship: 0,
                at: h(255, 0)
            }
        )
    );
    assert!(narrative(&g.0)
        .join("\n")
        .contains("runs off the board (row 256 col 1 h)"));

    // The reveal: Alice wins with a bent carrier, and her honest reveal proves it.
    let mut g = game(RuleSet::classic());
    let mut fleet = alice_fleet();
    fleet[0] = h(0, 7);
    let mut squares: Vec<u16> = ALICE_SHIPS[5..].to_vec();
    squares.extend(&bent);
    rig(&mut g, 0, &fleet, &occ_of(&squares));
    ok(&mut g, 1, &format!("{BOB} ready"));
    let water = water_of(&squares);
    duel(&mut g, [&BOB_SHIPS, &water], &mut honest);
    assert_eq!(g.1.state.provisional, Some(0));
    ok(&mut g, 0, "");
    assert_eq!(
        forfeit(&g),
        (
            1,
            0,
            Cheat::OffBoard {
                ship: 0,
                at: h(0, 7)
            }
        )
    );

    // The reveal, with row 255: Alice's own binary can't read it back, so she forges.
    let mut g = game(RuleSet::classic());
    let mut fleet = alice_fleet();
    fleet[0] = h(255, 0);
    rig(&mut g, 0, &fleet, &occ_of(&ALICE_SHIPS));
    ok(&mut g, 1, &format!("{BOB} ready"));
    let water = water_of(&ALICE_SHIPS);
    let mut answers =
        |st: &State, d: usize| (d == 0).then(|| answer_with(0, st.pending.unwrap(), false, None));
    duel(&mut g, [&BOB_SHIPS, &water], &mut answers);
    assert_eq!(g.1.state.provisional, Some(0));
    assert_eq!(
        duty_of(&g, 0),
        None,
        "an honest binary can't read row 255 back"
    );
    put(
        &mut g,
        0,
        Event::Reveal {
            b: Hex(seed_of(0)),
            fleet,
        },
    )
    .unwrap();
    assert_eq!(
        forfeit(&g),
        (
            1,
            0,
            Cheat::OffBoard {
                ship: 0,
                at: h(255, 0)
            }
        )
    );
}

#[test]
fn a_reveal_with_overlap_touching_or_phantom_forfeits() {
    // Overlap and Phantom under classic: Alice's cruiser rigged onto her battleship's row.
    let mut g = game(RuleSet::classic());
    let mut fleet = alice_fleet();
    fleet[2] = h(2, 2);
    let squares: Vec<u16> = ALICE_SHIPS.to_vec();
    rig(&mut g, 0, &fleet, &occ_of(&squares));
    ok(&mut g, 1, &format!("{BOB} ready"));
    let water = water_of(&ALICE_SHIPS);
    let mut lie = |st: &State, d: usize| {
        (d == 0).then(|| {
            let i = st.pending.unwrap();
            answer_with(0, i, ALICE_SHIPS.contains(&i), None)
        })
    };
    duel(&mut g, [&BOB_SHIPS, &water], &mut lie);
    ok(&mut g, 0, "");
    assert_eq!(
        forfeit(&g),
        (
            1,
            0,
            Cheat::Overlap {
                ship: 1,
                other: 2,
                cell: 22
            }
        )
    );

    let mut g = game(RuleSet::classic());
    let mut squares = ALICE_SHIPS.to_vec();
    squares.push(99);
    rig(&mut g, 0, &alice_fleet(), &occ_of(&squares));
    ok(&mut g, 1, &format!("{BOB} ready"));
    let water = water_of(&squares);
    duel(&mut g, [&BOB_SHIPS, &water], &mut honest);
    ok(&mut g, 0, "");
    assert_eq!(forfeit(&g), (1, 0, Cheat::Phantom { cell: 99 }));

    // Touching under notouch: Alice's legal-looking fleet whose rows touch.
    let mut g = game(RuleSet::notouch());
    let fleet = vec![h(0, 0), h(1, 0), h(4, 0), h(6, 0), h(8, 0)];
    let squares: Vec<u16> = vec![
        0, 1, 2, 3, 4, 10, 11, 12, 13, 40, 41, 42, 60, 61, 62, 80, 81,
    ];
    rig(&mut g, 0, &fleet, &occ_of(&squares));
    ok(
        &mut g,
        1,
        "place carrier A1 h battleship J7 h cruiser C6 v submarine E9 v destroyer H1 h ready",
    );
    let bob: Vec<u16> = vec![
        0, 1, 2, 3, 4, 96, 97, 98, 99, 25, 35, 45, 48, 58, 68, 70, 71,
    ];
    let water = water_of(&squares);
    duel(&mut g, [&bob, &water], &mut honest);
    assert_eq!(g.1.state.provisional, Some(0));
    ok(&mut g, 0, "");
    assert_eq!(forfeit(&g), (1, 0, Cheat::Touching { ship: 0, other: 1 }));
}

/// Commitments copied from the other seat fold fine, but nothing Bob can
/// sign opens them: his binary owes an answer it can't give, and waits.
#[test]
fn a_commitment_replayed_from_another_seat_or_session_never_opens() {
    let mut g = game(RuleSet::classic());
    ok(&mut g, 0, &format!("{ALICE} ready"));
    let alice = g.1.state.boards[0].clone();
    for (k, c) in alice.placed.iter().enumerate() {
        let c = Hex(c.unwrap());
        put(&mut g, 1, Event::Placed { ship: k as u8, c }).unwrap();
    }
    let cells = alice.cells.unwrap().into_iter().map(Hex).collect();
    put(&mut g, 1, Event::Fleet { cells }).unwrap();
    ok(&mut g, 0, "A1");
    assert_eq!(duty_of(&g, 1), None);
    let c = go(&mut g, 1, "");
    assert_eq!((c.failure, c.steps.len()), (None, 0));
    assert_eq!(
        Battleship::may_move(&g.1.state, 0, &g.1.names).unwrap_err(),
        "waiting on Bob's answer to the shot at A1"
    );
    let ledger = Battleship::render_as(
        &g.1.state,
        &g.1.names,
        Some(1),
        Some(&g.2[1].ctx),
        Format {
            id: "ledger",
            color: false,
        },
    );
    assert!(
        ledger.starts_with("error: can't read your carrier back from the log with this password\n")
    );
    // Neither Bob's own salt nor Alice's true one opens Alice's square under seat p2.
    for salt in [cell_salt(1, 0), cell_salt(0, 0)] {
        let mut s = g.0.clone();
        forge(
            &mut s,
            1,
            &Event::Answer {
                cell: 0,
                hit: true,
                salt,
                sunk: None,
            },
        );
        assert_eq!(
            broken(&s),
            "BROKEN at seq 16: p2's answer for A1 doesn't open p2's commitment for that square"
        );
    }
    // A board committed for another session doesn't open in this one.
    let elsewhere = board::cell_scope("TEST02", "p2");
    let theirs = slots::commit(&elsewhere, 0, &cell_salt(1, 0).0, &[1]);
    let here = locked().1.state.boards[1].cells.clone().unwrap()[0];
    assert_ne!(theirs, here);
}

#[test]
fn the_loser_owes_no_reveal() {
    let g = bob_wins_to_reveal();
    let mut s = g.0.clone();
    forge(
        &mut s,
        0,
        &Event::Reveal {
            b: Hex(seed_of(0)),
            fleet: alice_fleet(),
        },
    );
    let n = s.log.len() - 1;
    assert_eq!(
        broken(&s),
        format!("BROKEN at seq {n}: p1 owes no reveal: p1's fleet was sunk")
    );
    assert_eq!(duty_of(&g, 0), None);
    let mut s = g.0.clone();
    forge(&mut s, 0, &Event::Shot { cell: 0 });
    assert_eq!(
        broken(&s),
        format!("BROKEN at seq {n}: the battle is over: only final reveals now")
    );
}

#[test]
fn a_wrong_board_secret_doesnt_open() {
    let g = bob_wins_to_reveal();
    let mut s = g.0.clone();
    forge(
        &mut s,
        1,
        &Event::Reveal {
            b: Hex(seed_of(0)),
            fleet: bob_fleet(),
        },
    );
    let n = s.log.len() - 1;
    assert_eq!(
        broken(&s),
        format!("BROKEN at seq {n}: p2's board secret doesn't open p2's commitment for A1")
    );
    let mut s = g.0.clone();
    forge(
        &mut s,
        1,
        &Event::Reveal {
            b: Hex(seed_of(1)),
            fleet: bob_fleet()[..4].to_vec(),
        },
    );
    assert_eq!(
        broken(&s),
        format!("BROKEN at seq {n}: p2's reveal lists 4 ships; the fleet has 5")
    );
}

/// Forfeit plumbing: Over at once, the right winner, no reveals owed, and
/// nothing after it folds.
#[test]
fn a_forfeit_ends_the_game_at_once() {
    let mut g = locked();
    put(&mut g, 0, Event::Shot { cell: 0 }).unwrap();
    put(&mut g, 1, answer_with(1, 0, true, Some((4, h(0, 0))))).unwrap();
    assert_eq!(g.1.state.phase, Phase::Over);
    assert_eq!(Battleship::winner(&g.1.state), Some(0));
    assert_eq!(duty_of(&g, 0), None);
    assert_eq!(duty_of(&g, 1), None);
    assert_eq!(fails(&mut g, 0, "B5"), "the game is over");
    let mut s = g.0.clone();
    forge(&mut s, 0, &Event::Shot { cell: 5 });
    assert_eq!(broken(&s), "BROKEN at seq 17: the game is over");
    let text = narrative(&g.0);
    assert!(text.last().unwrap().contains("CHEAT: "), "{text:?}");
}

/// B§14: nothing on disk names an unshot square or an unsunk ship.
#[test]
fn the_disk_holds_no_board_secret() {
    use crate::engine::hw;
    use crate::storage::Store;
    let mut g = locked();
    let water = water_of(&ALICE_SHIPS);
    duel(&mut g, [&BOB_SHIPS[..4], &water[..4]], &mut honest);
    let root = std::env::temp_dir().join(format!("0sum-bs-disk-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let store = Store::at(root.clone());
    store.prepare().unwrap();
    assert!(store.create(&g.0).unwrap());
    for (i, p) in g.2.iter().enumerate() {
        store
            .write_hw("TEST01", seat(i), &hw::mark(&g.0, &p.sk))
            .unwrap();
    }
    let mut disk = String::new();
    for e in std::fs::read_dir(root.join("sessions")).unwrap() {
        disk.push_str(&std::fs::read_to_string(e.unwrap().path()).unwrap_or_default());
    }
    let st = &g.1.state;
    for who in 0..2 {
        let b = seed_of(who);
        assert!(!disk.contains(&hexs(&b)));
        let shots = &st.boards[who].shots;
        for i in 0..100u16 {
            let salt = hexs(&slots::salt(&b, CELL, i));
            assert_eq!(
                disk.contains(&salt),
                shots[i as usize].is_some(),
                "seat {who} square {i}"
            );
        }
        for k in 0..5 {
            let salt = hexs(&slots::salt(&b, SHIP, k as u16));
            assert_eq!(
                disk.contains(&salt),
                st.boards[who].sunk[k].is_some(),
                "seat {who} ship {k}"
            );
        }
    }
    assert!(st.boards[1].sunk[4].is_some() && st.boards[1].sunk[2].is_none());
    std::fs::remove_dir_all(&root).unwrap();
}

// ---------------------------------------------------------------- render formats

/// The mockups' position (look/notes.md), from Alice's seat at shot 31.
fn mockup() -> Seated {
    let mut g = game(RuleSet::classic());
    ok(
        &mut g,
        0,
        "place carrier A2 h battleship C9 v cruiser E2 h submarine H5 v destroyer I8 h ready",
    );
    ok(
        &mut g,
        1,
        "place battleship B3 h cruiser D7 v carrier J1 h submarine H7 h destroyer A9 h ready",
    );
    let sq = |s: &str| {
        let (r, c) = board::parse_square(&RuleSet::classic(), s).unwrap();
        r as u16 * 10 + c as u16
    };
    let alice: Vec<u16> = "A1 B3 B4 C3 C7 D7 D9 E5 E7 E10 F2 G7 H4 I8 F7"
        .split(' ')
        .map(sq)
        .collect();
    let bob: Vec<u16> = "A9 B3 B8 C5 D2 F6 G3 H1 H8 J1 J10 I8 I9 D9 E9"
        .split(' ')
        .map(sq)
        .collect();
    duel(&mut g, [&alice, &bob], &mut honest);
    assert_eq!(g.1.state.history.len(), 30);
    assert_eq!(Battleship::to_move(&g.1.state), Some(0));
    g
}

fn draw(g: &Seated, seat: usize, id: &'static str, color: bool) -> String {
    Battleship::render_as(
        &g.1.state,
        &g.1.names,
        Some(seat),
        Some(&g.2[seat].ctx),
        Format { id, color },
    )
}

fn look(name: &str) -> String {
    let path = format!("{}/tests/looks/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

#[test]
fn bridge_matches_look_3() {
    let g = mockup();
    assert_eq!(draw(&g, 0, "bridge", false), look("bridge.txt").trim_end());
    let color = draw(&g, 0, "bridge", true);
    assert!(color.contains("\x1b[1;38;5;203m✕\x1b[0m"));
    assert!(color.contains("\x1b[38;5;160m▓\x1b[0m"));
    assert!(color.contains("\x1b[38;5;117m○\x1b[0m"));
    let stripped = strip_ansi(&color);
    assert_eq!(stripped, draw(&g, 0, "bridge", false));
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::new();
    let mut esc = false;
    for c in s.chars() {
        match (esc, c) {
            (false, '\x1b') => esc = true,
            (true, 'm') => esc = false,
            (true, _) => {}
            (false, c) => out.push(c),
        }
    }
    out
}

#[test]
fn logbook_matches_look_1() {
    let g = mockup();
    assert_eq!(
        draw(&g, 0, "logbook", false),
        look("logbook.txt").trim_end()
    );
    assert_eq!(draw(&g, 0, "logbook", true), draw(&g, 0, "logbook", false));
    let t = &g.1;
    assert_eq!(
        Battleship::render(&t.state, &t.names, Some(0), Some(&g.2[0].ctx)),
        draw(&g, 0, "logbook", false)
    );
}

#[test]
fn ledger_is_one_fact_per_line() {
    let g = mockup();
    assert_eq!(
        draw(&g, 0, "ledger", false),
        "turn 31 · you vs Bob · your shot
your fleet: carrier A2-A6 ok · battleship C9-F9 hit D9 E9 · cruiser E2-E4 ok · submarine H5-J5 ok · destroyer I8-I9 SUNK
enemy shots: misses A9 B3 B8 C5 D2 F6 G3 H1 H8 J1 J10
your shots: open hits B3 B4 · sunk cruiser D7-F7 · misses A1 C3 C7 D9 E5 E10 F2 G7 H4 I8
enemy afloat: carrier 5 · battleship 4 · submarine 3 · destroyer 2
you afloat: carrier 5 · battleship 4 · cruiser 3 · submarine 3
last: you F7 sunk cruiser · enemy E9 hit
next: zs play <square>, e.g. zs play B5"
    );
    assert_eq!(draw(&g, 0, "ledger", true), draw(&g, 0, "ledger", false));
    let bob = draw(&g, 1, "ledger", false);
    assert!(
        bob.contains("\nyour shots: open hits D9 E9 · sunk destroyer I8-I9 · misses "),
        "{bob}"
    );
    assert!(bob.ends_with("\nnext: waiting on enemy's shot"), "{bob}");
}

/// H6: in the ledger the opponent's name is only ever on the header line.
#[test]
fn the_ledger_names_the_opponent_only_in_its_header() {
    let mut g = seated::<Battleship>(&RuleSet::classic(), &["you A1 hit", "Zed"]);
    let check = |g: &Seated, seat: usize| {
        let text = draw(g, seat, "ledger", false);
        let other = &g.1.names[1 - seat];
        let mut lines = text.lines();
        let head = lines.next().unwrap();
        if head.starts_with("placing") || head.starts_with("turn") {
            assert!(head.contains(&format!("· you vs {other} ·")), "{head}");
        }
        for l in lines {
            assert!(!l.contains(other.as_str()), "{other} leaked into `{l}`");
        }
    };
    for s in 0..2 {
        check(&g, s);
    }
    ok(&mut g, 0, &format!("{ALICE} ready"));
    ok(&mut g, 1, &format!("{BOB} ready"));
    let water = water_of(&ALICE_SHIPS);
    duel(&mut g, [&BOB_SHIPS[..3], &water[..2]], &mut honest);
    for s in 0..2 {
        check(&g, s);
    }
    duel(&mut g, [&BOB_SHIPS[3..], &water[2..]], &mut honest);
    assert_eq!(g.1.state.phase, Phase::Reveal);
    for s in 0..2 {
        check(&g, s);
    }
    ok(&mut g, 0, "");
    for s in 0..2 {
        check(&g, s);
    }
    assert!(draw(&g, 1, "ledger", false)
        .starts_with("over · enemy won: sank your fleet; enemy's fleet verified\n"));
    assert!(draw(&g, 0, "ledger", false)
        .starts_with("over · you won: sank enemy's fleet; your fleet verified\n"));
}

#[test]
fn every_format_draws_every_phase_without_escapes_where_promised() {
    let mut g = game(RuleSet::classic());
    let mut snaps = Vec::new();
    let mut snap = |g: &Seated| {
        for seat in 0..2 {
            for id in Battleship::FORMATS {
                snaps.push((*id, draw(g, seat, id, true)));
            }
        }
    };
    snap(&g);
    ok(&mut g, 0, "place carrier A1 h battleship C1 h");
    snap(&g);
    ok(&mut g, 0, "place auto ready");
    snap(&g);
    ok(&mut g, 1, &format!("{BOB} ready"));
    snap(&g);
    ok(&mut g, 0, "fire A1");
    snap(&g);
    put(&mut g, 1, answer_with(1, 0, true, Some((4, h(0, 0))))).unwrap();
    snap(&g);
    for (id, text) in &snaps {
        if *id != "bridge" {
            assert!(!text.contains('\x1b'), "{id}:\n{text}");
        }
        assert!(text.is_ascii() || *id != "logbook", "{text}");
    }
    let over = draw(&g, 1, "ledger", false);
    assert!(
        over.starts_with("over · you forfeit: you declared the destroyer sunk but "),
        "{over}"
    );
    let over = draw(&g, 0, "logbook", false);
    assert!(
        over.ends_with(
            "  Bob forfeits: Bob declared the destroyer sunk but A2 on it was never hit"
        ),
        "{over}"
    );
}

#[test]
fn placing_views_guide_the_next_move() {
    let mut g = game(RuleSet::classic());
    ok(&mut g, 0, "place carrier A2 h battleship C9 v");
    ok(&mut g, 1, "place carrier A1 h battleship C1 h cruiser E1 h");
    assert_eq!(
        draw(&g, 0, "ledger", false),
        "placing · you vs Bob · enemy placed 3 of 5, not locked
your fleet: carrier A2-A6 · battleship C9-F9 · cruiser unplaced · submarine unplaced · destroyer unplaced
next: zs play place cruiser <square> <h|v>, or zs play place auto; then zs play ready"
    );
    let log = draw(&g, 0, "logbook", false);
    assert!(
        log.contains("  C . . . . . . . . # .      [x] carrier     5  A2-A6 h\n"),
        "{log}"
    );
    assert!(log.contains("   >  cruiser     3  next\n"), "{log}");
    assert!(
        log.contains(
            "  next: zs play place cruiser <square> <h|v>, e.g. zs play place cruiser A1 v\n"
        ),
        "{log}"
    );
    let bridge = draw(&g, 0, "bridge", false);
    assert!(bridge.contains("■■■■■ carrier    A2-A6 h"), "{bridge}");
    assert!(bridge.contains("╰─ placing ▶ 2 of 5 placed ─"), "{bridge}");
    ok(&mut g, 0, "place auto ready");
    assert!(draw(&g, 0, "ledger", false).ends_with("\nnext: waiting on enemy to place their fleet"));
    assert!(
        draw(&g, 0, "bridge", false).contains("╰─ locked ▶ waiting on Bob to place their fleet ─")
    );
}

#[test]
fn battleship_formats_resolve_per_seat() {
    let prefs = |flag, env, tty, no_color| FormatPrefs {
        flag,
        env,
        tty,
        no_color,
    };
    let r = |p: FormatPrefs| resolve_format::<Battleship>(&p);
    let f = |id, color| Ok(Format { id, color });
    assert_eq!(r(prefs(None, None, true, false)), f("bridge", true));
    assert_eq!(r(prefs(None, None, false, false)), f("logbook", true));
    assert_eq!(r(prefs(None, None, true, true)), f("logbook", false));
    assert_eq!(
        r(prefs(Some("Bridge"), None, false, true)),
        f("bridge", false)
    );
    assert_eq!(
        r(prefs(None, Some("ledger"), true, false)),
        f("ledger", true)
    );
    assert_eq!(
        r(prefs(Some("logbook"), Some("ledger"), true, false)),
        f("logbook", true)
    );
    assert_eq!(
        r(prefs(None, Some("harbor"), true, false)),
        f("bridge", true)
    );
    assert_eq!(
        r(prefs(Some("harbor"), None, true, false)).unwrap_err(),
        "battleship has no `harbor` format (bridge, ledger, logbook)"
    );
    assert!(r(prefs(Some("ledge"), None, true, false)).is_err());
}
