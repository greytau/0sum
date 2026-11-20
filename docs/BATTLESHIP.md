# 0sum Battleship: a committed hidden board, provable answers, and a reusable slot commitment

> Design of 0sum Battleship. Written against `origin/main` @ `2885ba0` (phase 2 as built,
> `docs/PHASE2.md`), with the design review's amendments A1–A5 and A8 applied in the text and
> the render-format rulings folded in as B§9.1. Section references like "§3.4" without a prefix
> point into PHASE2; this document's own sections are written "B§n".
>
> **No breaking change.** Nothing here touches the envelope, the canonical bytes, the hash,
> the signatures, `log.rs`, `keys.rs`, `hw.rs` or `storage.rs`. Battleship rides in
> `Body::Game(Value)` like every game. The engine gains two default trait methods and one
> pure module (B§10). Every existing Farkle and Liar's Dice session verifies byte-identically.

---

## B§0. The shape in one breath

Each player derives a **board seed** `b` from their password. From `b` come independent
256-bit salts, one per slot. A player commits to two arrays of slots:

1. **Ship slots** (5): slot `k` hides ship `k`'s placement (origin + direction). Each one is
   appended as the player places that ship, so a half-placed fleet survives between runs with
   nothing private on disk: the binary reads its own placements back by trying the ≤ 200
   possible placements against its own salts.
2. **Cell slots** (100): slot `i` hides one bit, whether square `i` is occupied. They are
   published all at once when the player types `ready`. That locks the fleet.

A shot is public. The defender's binary answers it automatically on the defender's next run,
and the answer **is the opening of that square's cell slot**. Anyone can check it on the spot,
so hit and miss can't be lied about. Opening a hit reveals only "occupied", not which ship.
When a hit completes a ship, the answer also opens that ship's slot. That proves "sunk,
destroyer" on the spot and reveals the ship's position, as the real game does.

Two things can't be proven mid-game without zero-knowledge machinery: that the fleet is legal,
and that a "sunk" wasn't held back. Both are caught. Each sunk opening is checked against the
public shots. Counting rules apply (17 hits force 5 sunk; 84 misses prove a short fleet). And
at the end the winner reveals `b`, which opens everything on its board. **A lie that a well-formed entry
proves is a forfeit, not a broken log.** The liar loses and the honest player gets a recorded
win. A malformed entry (an "opening" that doesn't open) is BROKEN, exactly as in phase 2.

---

## B§1. Threat model: unchanged, plus what Battleship adds

The setting and G1–G6 from §1 hold as written. Battleship-specific guarantees:

| # | Property | Mechanism |
|---|----------|-----------|
| B1 | Reading the disk reveals nothing about either board before it is opened | Only commitments are stored. `b` and the salts come from the password and are never written down. Opened squares are public by design. |
| B2 | Nobody can move a ship after `ready` | Cell and ship slots are binding (SHA-256). After `fleet`, `fold` rejects any `placed`. |
| B3 | Every hit/miss answer is correct when it is given | The answer is the cell slot's opening; `fold` checks it before accepting. |
| B4 | Every "sunk, <ship>" is correct when it is given | The answer opens the ship slot; `fold` checks every square of it was opened as a hit, and that this shot is one of them. |
| B5 | A held-back "sunk" or an illegal fleet is proven, and the liar forfeits | Sunk-timing check (B4), the 17-hit and 84-miss counting rules, and the final reveal of `b` (B§4.5) |
| B6 | A commitment can't be replayed across seats, sessions, games or slots | Every slot hash binds label (game and array), SID, seat and index. `b` is per password, and a password is per seat per session. |

The non-guarantees are in B§12. The one to absorb now: **a cheater who is caught can still
refuse to make another move** (a stall, §12.3). Forfeit turns a lie into a loss only when the
liar's own binary appends the entry that proves it. A cheater who stops answering instead
denies the honest player a *recorded* win but never gets one of their own. The stall is
attributable.

---

## B§2. The slot commitment (`src/engine/slots.rs`, generic)

A committed array of slots with per-index openings. Battleship uses it twice. A future deck
(B§11) uses it the same way. It is pure functions over bytes, with no state and no I/O.

```
lp(x)     = u16be(len(x)) ‖ x                                    (as §4.1)

salt_j    = SHA-256( "0sum/v2/slot-salt\0" ‖ lp(label) ‖ seed ‖ u16be(j) )
C_j       = SHA-256( "0sum/v2/slot\0" ‖ lp(label) ‖ lp(SID) ‖ lp(seat) ‖ u16be(j) ‖ salt_j ‖ lp(v_j) )
opening_j = (j, salt_j, v_j)          checks iff C_j recomputes from it
```

- `label` = `<game-id>/<array>`. Battleship uses `battleship/ship` and `battleship/cell`.
  Labels are fixed strings in code, never taken from the log.
- `seed` is a 32-byte secret owned by one seat, revealed when the whole array is audited.
- **Why salts at all:** a cell holds one bit. An unsalted `H(i ‖ bit)` is opened by trying both
  values. Every slot gets its own 256-bit salt, so a commitment hides its value. Salts are
  independent hashes of the seed, so opening one slot (publishing `salt_j`) says nothing about
  any other slot, and nothing about the seed (preimage resistance).
- **Why derive salts from a seed** instead of HKDF-ing each one from the password: the final
  audit then needs one 32-byte reveal (`b`), and any verifier re-derives all 105 salts.
  HKDF outputs can't be re-derived without the password.
- **Flat list, not a Merkle root.** Flat costs 100 × 32 B once, then 32 B per answer. A Merkle
  root costs 32 B once, then about 7 × 32 B of path per answer, which adds up to more bytes over
  a game and more code. It gives nothing at this size. Revisit only for arrays in the thousands.

API (exactly this, nothing speculative):

```rust
pub struct Scope<'a> { pub label: &'a str, pub sid: &'a str, pub seat: &'a str }

pub fn salt(seed: &[u8; 32], label: &str, j: u16) -> [u8; 32];
pub fn commit(scope: &Scope, j: u16, salt: &[u8; 32], value: &[u8]) -> [u8; 32];
pub fn commit_all(scope: &Scope, seed: &[u8; 32], values: &[&[u8]]) -> Vec<[u8; 32]>;
pub fn opens(scope: &Scope, c: &[u8; 32], j: u16, salt: &[u8; 32], value: &[u8]) -> bool;
```

Liar's Dice's `dice.rs` is **not** migrated onto this. Its bytes are frozen by its vectors.

### B§2.1 Battleship's seed and encodings

```
b     = HKDF-Expand(prk, "0sum/v2/battleship/board", 32)        via Secrets::derive (§2.2)

ship slot k (k = fleet index, 0..5):  v = [row, col, dir]        dir: 0 = h (across), 1 = v (down)
cell slot i (i = row*cols + col):     v = [occ]                  occ: 0 = water, 1 = ship
```

`row`, `col` are 0-based. A placement's origin is its top/left square: `h` runs toward higher
columns, `v` toward higher rows. One game per session, so `b` has no round counter. A
rematch is a new session, which means a new SID and new passwords, so a new `b`.

---

## B§3. The fleet commitment

### B§3.1 Placement events

- `placed{ship:k, c:S_k}`: one per ship placement. A later `placed` for the same `k` (before
  `ready`) **replaces** it. Re-placing a ship you mistyped is free, because nothing is locked yet.
  The ship salt is the same each time. Every commitment hides its value until its slot is
  opened. Opening `S_k` (a sunk, or the reveal) also opens every earlier `placed{k}`, so the
  opponent learns that ship's abandoned positions and, from log order, that ships placed while
  it sat there avoid those squares. Re-placing to the same position repeats the same bytes.
  Accepted: it excludes a few placements of unsunk ships.
- `fleet{cells:[K_0 … K_99]}`: the 100 cell commitments, computed from the current placements.
  This **is** the lock. After it, `fold` rejects any `placed` from that seat (B2).

The final ship slot `S_k` is the last `placed{k}` before that seat's `fleet`.

### B§3.2 The partial board survives with nothing stored (no AEAD)

There is no private file and no encrypted state (B1, §5). On every run the binary **reads its
own placements back from the log**. For each `placed{k, S_k}` it tries every
`(row, col, dir)` with `row < rows`, `col < cols`, `dir ∈ {h, v}` (200 candidates on 10×10)
and keeps the one where `commit(scope_ship, k, salt(b, ship, k), [row, col, dir]) == S_k`.
That is at most 1000 SHA-256 per run. The opponent can't do this because they don't know `b`.

So `state_key` stays reserved and `chacha20poly1305` stays out (§5's "not before" still holds).
Deterministic recomputation already removes the need to store anything, which is §5's argument
carried one step further.

If a commitment matches no candidate (it wasn't made with this password's salts), the binary
reports `can't read your <ship> back from the log with this password` and appends nothing.

### B§3.3 `place auto` is random, not derived from the password

`place auto` draws from `ctx.rng` (OsRng-seeded `StdRng` in production, seeded in tests). It is
**not** deterministic from the password, and that is deliberate. Dice are a *draw* that rollback
must not re-roll. A placement is a *free choice*: `place carrier A1 h` already lets you pick any
legal fleet, so re-rolling `auto` by rollback gains nothing a player couldn't type. Determinism
would only make `place auto` impossible to re-roll on purpose, which is bad UX.

`place auto` places every ship not yet placed, uniformly at random among legal positions given
the ships already there. Ships go in fleet order. For each ship it enumerates every legal
placement and picks one with `rng.gen_range(0..n)`. If it reaches a dead end it restarts from
the board it started with, up to 100 times, then errors
`can't fit the remaining ships around the ones you placed; re-place some by hand`.
**If every ship is already placed, `place auto` re-rolls the whole fleet.** That one rule
covers pure auto, mixed manual-then-auto, and "I don't like this layout".

### B§3.4 Replay binding

`S_k` and `K_i` bind `label ‖ SID ‖ seat ‖ index`. Here is what each copy attack runs into:

- **Copying the opponent's commitments as your own:** your answers must open them under *your*
  seat, with salts from *their* `b`. You can't. The best you can do is stall.
- **Copying from another session or a rematch:** different SID, different password, different
  `b`. Same result.
- **Swapping slots** (answering square 5 with square 6's opening, or opening the destroyer's
  slot as the carrier): the index is inside the hash.

---

## B§4. The answer path: the central decision

### B§4.1 The options

| Option | Hit/miss trustworthy *during* play? | Cost | Verdict |
|---|---|---|---|
| (i) Bare claims, reveal at the end | **No.** A lie steers the game, and is caught only if the liar cooperates at the end. A losing liar just never reveals. | Tiny events | Rejected: fails the brief's core requirement |
| (ii) Per-cell openings against a flat commitment | **Yes**, checkable the instant it lands | 6.4 KB `fleet` entry per player, 32 B salt per answer | **Chosen** (with B§4.2) |
| (ii′) Same with a Merkle root | Yes | More bytes over a game, more code | Rejected (B§2) |
| (iii) Engine answers from an encrypted board | Only if the attacker's process can decrypt the defender's board, i.e. holds the defender's key. That breaks B1 and §12.4. | | Rejected |
| (iv) ZK proofs (fleet legality at commit, "not sunk" on each hit) | Yes, and legality too | Proof systems, heavy deps, a lot of code | Rejected as over-build; it's the only way to close B§12.3 |

**Pick: (ii), two-level.** Per-square openings make hit/miss provable. Per-ship slots make
"sunk" provable and keep ship identity hidden until the ship sinks. Option (i)'s end reveal is
kept, but only to audit what (ii) can't prove mid-game: legality and sunk timing.

### B§4.2 What an opening reveals, versus the classic rules

- **Miss:** the square is water. Classic.
- **Hit:** the square is occupied. **Not which ship.** The cell array carries one bit, and the
  ship identity lives only in the separate ship array. Classic ("hit", nothing more).
- **Sunk:** the answer to the shot that completes ship `k` also carries `sunk{ship:k, salt, at}`,
  the opening of `S_k`. That proves the name ("you sank my destroyer") and reveals that ship's
  squares. Classic play reveals those too (you've hit every one of them), so nothing extra
  leaks. `fold` checks, in order (B§6.2):
  1. `S_k` opens. Otherwise BROKEN.
  2. The placement is on the board, this shot is one of its squares, and every one of its squares
     has been opened as a hit. It doesn't overlap or (under `notouch`) touch an already-sunk
     ship. Otherwise **forfeit**.

  Check 2's "this shot is one of its squares" is what catches a **held-back sunk**. If the ship
  was completed by an earlier shot and declared later, the declaration proves the earlier
  answer lied by omission.

### B§4.3 Proven lie = forfeit; malformed = BROKEN

- **BROKEN** (`fold` returns `Err`): the entry is not a valid protocol move. An "opening" that
  doesn't open, an answer to a shot that wasn't fired, a second lock, a reveal before the end.
  An honest binary can never produce one. As in §3.4, every honest binary then refuses to
  append, and `verify` exits 1 naming the seq, whose author signed it.
- **Forfeit** (`fold` returns `Ok`, the game ends): the entry is a well-formed opening, but what
  it opens, together with the public record, proves the author lied, either at `ready` (an
  illegal fleet) or in an earlier answer (a withheld sunk). `fold` sets
  `outcome = Forfeit{liar, cheat}` and the other seat wins. `verify` prints
  `CHEAT: <reason>. <liar> forfeits; <other> wins.` in the narrative, then `OK …` (the log is
  intact; it is the *proof*).

Why the split matters: if a proven lie were BROKEN, every honest binary would refuse the log
and the game would freeze with no winner. Lying would earn a flipped table instead of a loss.
A forfeit gives the honest player a win recorded in the very log that proves the cheat.
**If it opens, it counts; if what it opens proves a lie, its author loses.**

When more than one lie exists, **the first lie proven, in log order, decides.** The game is
over at that entry.

### B§4.4 Fleet legality: when it's checked, and how the game ends

The fleet (5/4/3/3/2, straight, on the board, no overlap, and no touching under `notouch`)
can't be proven at `ready` without ZK. It is checked at three moments:

1. **On every sunk opening** (B§4.2 check 2): that ship is on the board and doesn't collide
   with any ship already sunk.
2. **By counting**, on every answer (`F` = fleet squares = 17, `N` = rows×cols = 100):
   - **`hits == F`**: on a legal board every ship is now sunk. If all 5 are declared, the
     attacker has won provisionally and the game moves to the **Reveal** phase. If fewer are
     declared, the defender **forfeits** (`UnsunkAtFull`). So the attacker's win condition is
     public and needs no trust: **17 hits**.
   - **`misses > N − F`** (84 misses): the board can't hold 17 occupied squares, so the defender
     **forfeits** (`TooManyMisses`).
   - Every board is shot out within 100 shots (`hits + misses ≤ 100`), so one of these two
     always fires. A rigged board can't make the game unwinnable for the attacker.
3. **At the final reveal** (B§4.5), in full.

**The 16-square board, worked through.** Bob commits one of his carrier's squares as water.
Shooting it opens "miss", truthfully, because that's what he committed. The carrier can now
never be declared sunk: a declaration fails check 2 and Bob forfeits. Alice can reach at most
16 hits, so she can't win by sinking. She wins by forfeit at Bob's 84th miss, or at the reveal
if Bob "wins" first, or Bob stalls (B§12.3). Bob never gets a confirmed win.

### B§4.5 The end: the winner's fleet is revealed

When the battle ends by `hits == F` with all 5 sunk, the phase becomes **Reveal** and the
attacker is the **provisional** winner. Only the provisional winner owes `reveal{b, fleet}`: its
board seed and its five placements, in fleet order. The loser owes nothing: its five sunk
openings already proved its fleet. `fold` re-derives every salt from `b` and checks:

1. **Opens** (BROKEN otherwise): each `S_k` opens with `fleet[k]`, and each `K_i` opens with
   `occ ∈ {0, 1}`. That recovers the full committed board.
2. **Legal** (forfeit otherwise), in this order: each ship on the board (`OffBoard`), no
   overlaps (`Overlap`), no touching if `!touching` (`Touching`), and committed occupancy equals
   the union of the five ships, checked square by square (`Phantom` = occupied but no ship;
   `Hidden` = a ship's square committed as water).
3. **Sunk timing** (forfeit otherwise): no ship whose squares were all hit went undeclared
   (`Withheld`).

The first failure ends the game for the other seat. When the reveal passes, the phase is
**Over**, the provisional winner wins, and the narrative says the winner's fleet was verified.

A forfeit mid-battle (B§4.2, B§4.4) ends the game **at once, with no reveals.** The proof is
already in the log.

### B§4.6 A defender who never answers

That's a stall (§12.3). It is an attributable wait (`waiting on Bob's answer to the shot at
B5`), with no timeout, because there is no trusted clock. The answer is a **duty**: it fires on
*any* invocation by the defender, `zs play` with no moves included. So withholding one takes a
modified binary or deliberately never running `0sum` again. The same goes for a reveal that
never comes. A provisional winner who never reveals shows as
`Alice sank Bob's fleet; waiting on Alice's final reveal to confirm`, and the win is never final.

---

## B§5. Rules as data

`src/battleship/ruleset.rs`, in the style of Farkle's and Liar's Dice's:

```rust
//! Declarative Battleship rules. Pure data; mechanics read every variant from here.

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShipKind { pub name: String, pub len: u8 }

/// Stored in genesis so every session is self-describing and reproducible.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleSet {
    pub name: String,
    pub rows: u8,              // ≤ 26 (row letters)
    pub cols: u8,              // ≤ 26
    /// Fleet order is slot order (ship index k).
    pub fleet: Vec<ShipKind>,
    /// Ships may share an edge or corner. When false, no two ships are 8-adjacent.
    pub touching: bool,
}

impl RuleSet {
    pub fn by_name(name: &str) -> Result<RuleSet, String> {
        match name.to_ascii_lowercase().as_str() {
            "classic" => Ok(RuleSet::classic()),
            "notouch" => Ok(RuleSet::notouch()),
            other => Err(format!("unknown ruleset `{other}` (available: classic, notouch)")),
        }
    }
    /// The default. 10×10, carrier 5, battleship 4, cruiser 3, submarine 3, destroyer 2; touching allowed.
    pub fn classic() -> RuleSet { /* … touching: true */ }
    /// Same fleet, ships may not touch, not even diagonally.
    pub fn notouch() -> RuleSet { RuleSet { name: "notouch".into(), touching: false, ..RuleSet::classic() } }
    pub fn fleet_cells(&self) -> u16 { self.fleet.iter().map(|s| s.len as u16).sum() }
    pub fn cells(&self) -> u16 { self.rows as u16 * self.cols as u16 }
}
```

- **`notouch` is in** because it really is trivial: one 8-neighbour check, used by `plan`,
  `place auto`, the sunk check and the reveal. It also gives the abstraction a second ruleset,
  as LD has `natural`.
- **`salvo`: no.** Classic salvo reports *how many* shots of a salvo hit, not *which*.
  Per-square openings necessarily say which, so we'd ship a different game under salvo's name,
  and the faithful version needs ZK. The per-shot variant ("open salvo") would add a pending
  queue, partial-turn state and a salvo-size rule for no protocol gain. If it's ever wanted, it's
  additive: a `shots_per_turn` field and `pending: Vec<u16>`. Don't add the field now.
- **Fixed rules (not variants):** 2 players. `p1` fires first. One shot per turn, and a hit
  doesn't earn another. The answer carries the result. Win = 17 hits with 5 sunk, the winner's
  fleet revealed legal. Or the other seat forfeits.

### B§5.1 Coordinates and names

- **Squares:** row letter then column number, `A1` … `J10`, case-insensitive. Parse
  `^[A-Za-z][0-9]{1,2}$`, row = letter − `A` < rows, col = number − 1 < cols. Index
  `i = row*cols + col`. Display is always uppercase (`B5`). The log stores the index.
- **Direction:** `h` (aliases `across`, `horizontal`) or `v` (aliases `down`, `vertical`). The
  origin is the top/left end.
- **Ships:** a ship token matches a fleet name case-insensitively, or a **unique prefix** of
  one: `b` → battleship, `ca` → carrier, `cr` → cruiser, `s` → submarine, `d` → destroyer.
  `c` is ambiguous and gets `` `c` could be carrier or cruiser — type more letters ``. This is
  data-driven, so a future fleet needs no alias table.

---

## B§6. State, events, fold (normative)

### B§6.1 Events

```rust
#[derive(Serialize, Deserialize)] #[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Event {
    Placed { ship: u8, c: Hex32 },
    Fleet  { cells: Vec<Hex32> },
    Shot   { cell: u16 },
    Answer { cell: u16, hit: bool, salt: Hex32, sunk: Option<Sunk> },   // None serializes as null
    Reveal { b: Hex32, fleet: Vec<Placement> },
}
#[derive(Serialize, Deserialize)] #[serde(deny_unknown_fields)]
pub struct Sunk { pub ship: u8, pub salt: Hex32, pub at: Placement }
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq)] #[serde(deny_unknown_fields)]
pub struct Placement { pub row: u8, pub col: u8, pub dir: Dir }
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
pub enum Dir { #[serde(rename = "h")] H, #[serde(rename = "v")] V }   // slot byte: H = 0, V = 1
```

JSON as it lands in `body.game`:
```json
{ "placed": { "ship": 4, "c": "3146c1…beff" } }
{ "fleet":  { "cells": ["335ec1…3861", "fa676b…a312", "… 100 entries …"] } }
{ "shot":   { "cell": 0 } }
{ "answer": { "cell": 1, "hit": true, "salt": "c07fed…0d05",
              "sunk": { "ship": 4, "salt": "0b1a8b…c7b6", "at": { "row": 0, "col": 0, "dir": "h" } } } }
{ "reveal": { "b": "4f471c…21e6", "fleet": [ { "row": 2, "col": 2, "dir": "v" }, "… 5 entries …" ] } }
```

Answers carry `hit` because it *is* the opened value (the `occ` byte). There is no separate
claim that could disagree with it. That is the same rule as LD's derived dice and Farkle's
dropped `points`.

### B§6.2 State

```rust
pub enum Phase { Setup, Battle, Reveal, Over }

pub struct Board {                         // one per seat; all public
    pub placed: Vec<Option<[u8; 32]>>,     // S_k, latest per ship
    pub cells:  Option<Vec<[u8; 32]>>,     // K_i; Some = fleet locked
    pub shots:  Vec<Option<bool>>,         // per square, as answered: Some(hit)
    pub sunk:   Vec<Option<Placement>>,    // per ship, once proven
    pub hits: u16, pub misses: u16,
    pub revealed: bool,
}
pub struct ShotRecord { pub by: usize, pub cell: u16, pub hit: Option<bool>, pub sunk: Option<u8> }

pub struct State {
    pub rules: RuleSet, sid: String, seats: Vec<String>,
    pub phase: Phase,
    pub boards: Vec<Board>,                // [p1's, p2's]: boards[d] is d's own fleet
    pub turn: usize,                       // the attacker
    pub pending: Option<u16>,              // turn's shot at boards[1 - turn], unanswered
    pub history: Vec<ShotRecord>,          // every shot in order, for render and recap
    pub provisional: Option<usize>,        // set on entering Reveal
    pub outcome: Option<Outcome>,          // set on entering Over
}
pub struct Outcome { pub winner: usize, pub how: How }
pub enum How { Sank, Forfeit { liar: usize, cheat: Cheat } }
pub enum Cheat {                           // structured; narrate words it with names (B§8.2)
    OffBoard { ship: u8 }, Overlap { ship: u8, other: u8, cell: u16 }, Touching { ship: u8, other: u8 },
    SunkEarly { ship: u8, cell: u16 }, SunkLate { ship: u8, cell: u16 },
    UnsunkAtFull { sunk: u8 }, TooManyMisses { misses: u16 },
    Phantom { cell: u16 }, Hidden { ship: u8, cell: u16 }, Withheld { ship: u8 },
}
```

`a` = author and `d = 1 − turn` = defender. "Forfeit(x, cheat)" means: set
`outcome = Outcome{winner: 1 − x, how: Forfeit{liar: x, cheat}}`, set `phase = Over`, and
return `Ok`.

| Event by `a` | Valid iff (else `Err` → BROKEN) | Effect |
|---|---|---|
| `placed{k,c}` | `Setup`, `boards[a].cells` is None, `k < fleet.len()` | `placed[k] = c` (replaces) |
| `fleet{cells}` | `Setup`, not locked, every `placed[k]` set, `cells.len() == rows*cols` | lock. If both locked → `Battle`, `turn = 0` |
| `shot{i}` | `Battle`, `a == turn`, `pending` None, `i < cells`, `boards[1−a].shots[i]` None | `pending = Some(i)`; push `ShotRecord` |
| `answer{i,hit,salt,sunk}` | `Battle`, `pending == Some(i)`, `a == d`, `opens(cell scope of a, K_i, i, salt, [hit])`; if `sunk`: `k < n`, `sunk[k]` None, `opens(ship scope of a, S_k, k, salt, enc(at))` | B§6.3 |
| `reveal{b,fleet}` | `Reveal`, `a == provisional` (else BROKEN `p2 owes no reveal: p2's fleet was sunk`), `!revealed`, `fleet.len() == n`, every `K_i` opens with occ 0 or 1 under `salt(b, cell, i)`, every `S_k` opens with `fleet[k]` under `salt(b, ship, k)` | B§6.4 |

Anything in `Over` → `Err("the game is over")`.

### B§6.3 Answer effect, in order

1. Record `shots[i] = Some(hit)`; bump `hits` or `misses`; fill in the `ShotRecord`.
2. If `sunk = Some{k, at}`, apply these in order, and the first that applies decides
   (squares come from `board::squares`, B§10.5; a sunk on a miss lands here too, as
   `SunkEarly` when shot `i` is on `at`, since `shots[i] = Some(false)`, else `SunkLate`):
   `at` off the board → Forfeit(a, `OffBoard{k}`) ·
   some square `j` of `at` with `shots[j] != Some(true)` → Forfeit(a, `SunkEarly{k, j}`) ·
   `i ∉ cells(at)` (so the ship was complete before this shot) → Forfeit(a, `SunkLate{k, i}`) ·
   `at` shares a square `j` with a sunk ship `m` → Forfeit(a, `Overlap{k, m, j}`) ·
   `!touching` and `at` is 8-adjacent to a sunk ship `m` → Forfeit(a, `Touching{k, m}`).
   Otherwise `sunk[k] = Some(at)`.
3. If `hits == fleet_cells`: if every ship is sunk → `provisional = Some(turn)`, `phase = Reveal`,
   `pending = None`. Otherwise Forfeit(a, `UnsunkAtFull{sunk count}`).
4. Else if `misses > cells − fleet_cells` → Forfeit(a, `TooManyMisses{misses}`).
5. Else `pending = None`, `turn = a` (the defender fires next).

### B§6.4 Reveal effect, in order

With the board recovered from `b` (`occ_i` for every square, `fleet[k]` for every ship; squares
from `board::squares`, B§10.5): `OffBoard` (each `k` in order) → `Overlap` (pairs in index order, report the lowest shared
square) → `Touching` if `!touching` → for each square `i` in order: `Phantom{i}` if
`occ_i = 1` and no ship covers it, `Hidden{k, i}` if ship `k` covers it and `occ_i = 0` →
`Withheld{k}` for the first undeclared ship whose squares all have `shots == Some(true)`.
The first one found is Forfeit(a, it). Otherwise `revealed = true`,
`outcome = Outcome{winner: provisional, how: Sank}`, `phase = Over`.

`fold` never brute-forces anything. The reveal lists the placements, so verifying is a fixed
105 + 100 hashes. Only the *owner's* binary brute-forces (B§3.2), and only its own slots.

### B§6.5 The rest of the trait

- `to_move` = `Some(turn)` in `Battle` with `pending` None, else None.
- `winner` = `outcome.map(|o| o.winner)`, so it is Some **only in Over**. A provisional win
  doesn't halt `apply_chain` with "the game is over" before reveal duties run.
- `may_move` (new, B§10): in `Setup`, `Ok` unless my fleet is locked, in which case
  `Err("your fleet is locked; waiting on <names> to place theirs")`. Otherwise the default.
- `waiting`: Setup → `<names> to place their fleet` · Battle → `<d>'s answer to the shot at B5`
  · Reveal → `<winner>'s final reveal` · Over → `nobody: the game is over`.
- `game_over` (Over only): `Alice won: sank Bob's fleet; Alice's fleet verified` or
  `Alice won: Bob forfeits — <cheat words>`. Any `play` on a finished game prints it and
  succeeds, as Farkle does.
- `recap` (the author's own event): `placed` → `placed the <ship>` · `fleet` → `locked the fleet`
  · `shot` → `fired at <sq>` · `answer`, `reveal` → None (protocol).
  `moment` → `shot <n>` in Battle (n = `history.len() + 1`), else None.
- `duty`, in order:
  1. `Battle`, `pending` is a shot at me → `answer`. Read my fleet back (B§3.2), `hit = occ[i]`,
     `salt = salt(b, cell, i)`. If `hit` and every square of the ship covering `i` is now hit,
     include `sunk{k, salt(b, ship, k), at}`.
  2. `Reveal`, I am `provisional`, not revealed → `reveal{b, my placements}`.

  If my own fleet can't be read back, `duty` returns None and `render` shows the B§3.2 error.
  A duty must never hand `fold` an event it rejects.
- `plan`: `place` → read back, validate (B§7.1), `[placed{k, S_k}]` · `place auto` → B§3.3, one
  `placed` per ship placed · `ready` → `[fleet{cells}]` from the read-back board, `Err` if any
  ship is unplaced · `fire` → `[shot{i}]`. Let `fold` produce the out-of-turn and duplicate errors.

---

## B§7. CLI, duties, and the flow

### B§7.1 Moves

```
0sum new battleship [--ruleset classic|notouch]   aliases: bs, battleships. Exactly 2 players.
zs play place <ship> <sq> <h|v> [<ship> <sq> <h|v> …]   one ship per triple; re-placing is fine until `ready`
zs play place auto                                 fill unplaced ships at random (re-roll all if none unplaced)
zs play ready                                      lock the fleet (aliases: lock, done)
zs play fire <sq>                                  aliases: f, shoot, or just the square: `zs play B5`
zs play                                            duties, then status
```

- **Chaining** follows the existing grammar: every verb starts a move, and after `place` each
  further triple is its own move. `place auto ready`, `place carrier A1 h ready`, and (when the
  opponent is already locked and you're `p1`) `place auto ready B5` all work in one run.
  `fire B5 fire B6` fires once, then halts with `waiting on Bob's answer to the shot at B5`,
  keeping the legal prefix as in §9.4.
- **Placement errors** come from `plan`, worded for humans:
  `the carrier doesn't fit at H8 across (5 long; the board ends at column 10)` ·
  `the carrier at A1 h overlaps your battleship at A3` ·
  `the cruiser at B1 h touches your carrier (ruleset notouch keeps ships apart)` ·
  `` `K1` isn't on the board (rows A-J, columns 1-10) `` ·
  `` unknown ship `x` (carrier, battleship, cruiser, submarine, destroyer) `` ·
  `place takes <ship> <square> <h|v>, e.g. \`place carrier A1 h\`, or \`place auto\``.
  Also `place the cruiser and submarine first (or place auto)` at `ready`, and
  `you already fired at B5`.
- **Placement opens once both seats have registered**, because the engine already rejects game
  entries before then (§3.4 step 5). `play` before that says
  `not everyone has registered yet` as today.

### B§7.2 Two players, enforced

`new` doesn't prompt for a count. It prints `Battleship is a 2-player game.` and asks for two
names. Genesis is checked against the game's seat range in `new_session` and in replay
(B§10): a doctored 3-seat battleship genesis is `BROKEN at seq 0: need exactly 2 players`.

### B§7.3 A game, exactly

Session `TEST01`, ruleset `classic`. Alice is `p1` and Bob is `p2`, with the §13 test passwords.
Fleets (the B§14 vector fleets):
Alice: carrier A1 h, battleship C1 h, cruiser E1 h, submarine G1 h, destroyer I1 h.
Bob: carrier C3 v, battleship J7 h, cruiser A6 v, submarine E8 v, destroyer A1 h.

| # | Who | Command | Entries (seq · author · body) | Prints |
|---|---|---|---|---|
| 1 | anyone | `0sum new battleship` (Alice, Bob) | `0 · — · genesis` | SID, register lines |
| 2 | Alice | `0sum register TEST01 Alice` | `1 · p1 · register` | password, hint, waiting on Bob |
| 3 | Bob | `0sum register TEST01 Bob` | `2 · p2 · register` (no duties in Setup) | password; place your fleet |
| 4 | Alice | `zs play place carrier A1 h battleship C1 h cruiser E1 h submarine G1 h destroyer I1 h ready` | `3–7 · p1 · placed{0..4}`, `8 · p1 · fleet{100}` | own board; locked; waiting on Bob |
| 5 | Bob | `zs play place carrier C3 v battleship J7 h cruiser A6 v submarine E8 v destroyer A1 h ready` | `9–13 · p2 · placed{0..4}`, `14 · p2 · fleet{100}` → **Battle**, Alice fires first | own board; Alice's turn |
| 6 | Alice | `zs play A1` | `15 · p1 · shot{0}` | waiting on Bob's answer to A1 |
| 7 | Bob | `zs play E5` | duty `16 · p2 · answer{0, hit, salt, sunk: null}`, then `17 · p2 · shot{44}` | `Alice fired at A1: hit` (his render adds: on your destroyer); waiting on Alice's answer |
| 8 | Alice | `zs play` | duty `18 · p1 · answer{44, miss, salt}` | **your shot at A1: hit**; Bob fired at E5: miss; your turn |
| 9 | Alice | `zs play A2` | `19 · p1 · shot{1}` | waiting on Bob's answer to A2 |
| 10 | Bob | `zs play` | duty `20 · p2 · answer{1, hit, salt, sunk{4, salt, A1 h}}` | `Alice sank your destroyer`; your turn |
| … | | | alternating as rows 7–10 | |
| m | Bob | `zs play` | duty `n · p2 · answer{99, hit, salt, sunk{1, salt, J7 h}}` → 17 hits, 5 sunk → **Reveal**, provisional Alice | `Alice sank your whole fleet`; waiting on Alice's final reveal |
| m+1 | Alice | `zs play` | duty `n+1 · p1 · reveal{b_p1, fleet}` (legal) → **Over** | `Alice won: sank Bob's fleet; Alice's fleet verified` |

**Invocations.**
- **Setup:** one run per player after registering. The `ready` that completes the pair starts
  the battle, and if that run is `p1`'s it can fire in the same command.
- **Battle:** one run per turn is required: `zs play <sq>` runs the answer duty for the
  opponent's last shot, then fires. A player who wants to *see* their last result before aiming
  takes a look first (row 8), and that look is where their answer duty runs, so the opponent sees
  their own result sooner. So it's 1 run per turn at minimum and 2 typically. That's the same
  floor as §7.3's: the result of your shot exists only after your opponent's run, and nothing
  but your next run can show it to you without a background co-signer (`0sum watch`, out of scope).
- **End:** The winner's next run reveals.

---

## B§8. `0sum verify`

### B§8.1 What it checks, in order

1. §3.4 steps 1–4, unchanged. Genesis also checks the game's seat range (B§10): `need exactly 2 players`.
2. Each game entry, through `G::fold`, in log order (§3.4 step 5). For Battleship that means:
   1. Setup: `placed` and `fleet` shape and order. Nobody fires before both fleets are locked,
      and no ship moves after its fleet is locked (B2).
   2. Every shot: in turn, at an unshot square, never two pending.
   3. **Every answer opens the defender's committed square** (B3). Every sunk opens the
      defender's committed ship, and its content is checked (B4).
   4. The counting rules after every answer (B§4.4).
   5. **The winner's reveal opens all 105 of its slots.** The fleet is then legal, it matches the
      committed squares, and every sunk was declared on time (B§4.5).
3. The narrative ends with the verdict line from the deciding entry, then `OK  <N> entries  head …`.

So on a finished, unforfeited game, `OK` means: every answer either player gave matched a board
committed before the first shot, the winner's board was a legal fleet, each of the loser's
five ships was proven on the shot that sank it, and every sunk was announced on the shot that
completed it. On a game in progress, `OK` proves everything up to the head, which means every
answer so far. Legality is then pending the reveal.

### B§8.2 Messages

**BROKEN** (fold `Err`, printed as `BROKEN at seq <i>: <reason>`; seat ids as LD does):

```
the battle hasn't started                                    (shot/answer/reveal in Setup)
p1 moved a ship after locking the fleet                      (placed after own fleet)
no ship #<k> in this fleet
p1 locked the fleet twice
p1 locked the fleet before placing the <ship>
p1's fleet has <n> square commitments; the board has <cells>
p2 fired out of turn
p1 fired again before p2 answered
square <i> isn't on the board
p1 already fired at <sq>
no shot is waiting for an answer
only p2 can answer the shot at <sq>
p2 answered <sq> but the shot was at <sq>
p2's answer for <sq> doesn't open p2's commitment for that square
p2's <ship> is already sunk
p2's sunk <ship> doesn't open p2's <ship> commitment
the battle is over: only final reveals now                   (shot/answer in Reveal)
reveal before the battle is over
p2 owes no reveal: p2's fleet was sunk
p1's reveal lists <n> ships; the fleet has <m>
p1's board secret doesn't open p1's commitment for <sq>
p1's revealed <ship> doesn't open p1's <ship> commitment
the game is over
```

**Forfeits** (`Ok`; narrate prints `CHEAT: <words>. <liar> forfeits; <other> wins.`):

```
OffBoard      <Bob>'s <carrier> runs off the board (<sq> <h|v>)        (a row past Z is spelled `row 256 col 1`)
Overlap       <Bob>'s <cruiser> overlaps the <submarine> at <sq>
Touching      <Bob>'s <cruiser> touches the <submarine> (ruleset notouch)
SunkEarly     <Bob> declared the <ship> sunk but <sq> on it was never hit
SunkLate      <Bob> declared the <ship> sunk on the shot at <sq>, which isn't on it: the sunk was held back
UnsunkAtFull  17 hits on <Bob>'s board but only <n> ships declared sunk
TooManyMisses <n> misses on <Bob>'s board: it can't hold a 17-square fleet
Phantom       <Bob>'s board marks <sq> as a ship but no ship is there
Hidden        <Bob>'s <carrier> covers <sq> but the board marks it as water
Withheld      <Bob>'s <battleship> was hit on every square but never declared sunk
```

(`17` and `84` are `fleet_cells` and `cells − fleet_cells + 1` from the ruleset. Don't hard-code them.)

**Narrative** (public, no secrets): `Alice placed the carrier` · `Alice locked the fleet` ·
`Both fleets locked. Alice fires first.` · `Alice fires at A1` · `Bob: A1 — hit` ·
`Bob: A2 — hit, Alice sank Bob's destroyer` ·
`Alice sank Bob's fleet; waiting on Alice's final reveal to confirm` · `Alice reveals: carrier A1 h,
battleship C1 h, cruiser E1 h, submarine G1 h, destroyer I1 h — legal, every answer matched` ·
`Alice won: sank Bob's fleet; Alice's fleet verified`.

---

## B§9. Rendering: the data, not the look

This section fixes what any look receives; B§9.1 is the looks. `render` is split in two:

```rust
fn render_as(st, names, me, ctx, format) -> String { draw_<format>(&view(st, me, ctx), names) }
```

```rust
pub struct View {
    pub rows: u8, pub cols: u8,
    pub fleet: Vec<(String, u8)>,             // (name, len) in fleet order
    pub ruleset: String, pub touching: bool,  // for the Touching words and placement examples
    pub me: Option<usize>,                    // None = no password (never used by verify today)
    pub phase: ViewPhase,
    pub own: Option<OwnView>,                 // needs ctx: my ships read back from the log
    pub target: Vec<TargetSquare>,            // my shots at the opponent, rows*cols, row-major
    pub fleets: [SideStatus; 2],              // public per-seat ship status, [me, opponent] when me is Some
    pub last_mine: Option<ShotView>,          // my most recent shot and its answer (or pending)
    pub last_theirs: Option<ShotView>,        // their most recent shot at me and my answer
    pub error: Option<String>,                // e.g. B§3.2's read-back failure
}
pub enum ViewPhase {
    Placing { locked: bool, opponent_placed: u8, opponent_locked: bool, next: Option<u8> }, // next unplaced ship
    Battle  { my_turn: bool },                        // a pending shot is `last_mine` with no result
    Reveal  { provisional: usize, waiting_on: Vec<usize> },   // waiting_on = [provisional]
    Over    { winner: usize, how: HowView },          // Sank | Forfeit { liar, cheat }
}
pub struct OwnView { pub squares: Vec<OwnSquare>, pub placed: Vec<Option<Placement>> }   // rows*cols
pub struct OwnSquare { pub ship: Option<ShipPart>, pub incoming: Incoming }
pub struct ShipPart { pub ship: u8, pub part: u8, pub dir: Dir }   // part 0 = bow/top-left … len-1; lets a look draw shapes
pub enum Incoming { None, Pending, Miss, Hit }                      // the opponent's shots at me; sunk = ship's entry in fleets[me]
pub enum TargetSquare { Unknown, Pending, Miss, Hit, Sunk(ShipPart) }   // sunk squares carry shape from the opening
pub struct SideStatus { pub sunk: Vec<Option<Placement>>, pub hits: u16, pub misses: u16, pub locked: bool }
pub struct ShotView { pub cell: u16, pub result: Option<(bool, Option<u8>)> }   // (hit, sunk ship) once answered
```

This covers the placement view (own board, placed and unplaced ships, the next one, the
opponent's progress), the battle view (own fleet with incoming hits and misses, target grid
with miss/hit/sunk-with-shape, both last shots, ships left on each side from
`fleets[*].sunk`), and the end views. Everything except `own` is public. `own` exists only with
`ctx`. `View` carries no names and no words: `Battle` has no `waiting` string and `HowView`
carries the structured `Cheat`, so each look words them itself (B§9.1's ledger says `enemy`
where the others say the opponent's name).

### B§9.1 Render formats: per seat, display only

**R1. The invariant.** A format is a pure function `draw_<fmt>(&View, names) -> String`. It
receives ONLY `View` (plus names), never `State`, never `Ctx`. `View` is the entitlement
boundary: whatever a seat may know is in it, and nothing else. So every format shows at most what
the seat is entitled to, by construction. A format may show less than another (it may omit),
never more. Render format never touches the signed log, game state, `fold`, `duty`, or `verify`.

**R2. Formats.**

| id | what | default when |
|---|---|---|
| `bridge` | rounded box, `· ■ ✕ ▓ ○`, ship roster under each board with damage, 256-color fg | stdout is a TTY and `NO_COLOR` is unset/empty |
| `logbook` | pure ASCII, `. # X = o`, no escapes | stdout not a TTY, or `NO_COLOR` set |
| `ledger` | terse coordinate text for LLM players (R4) | never automatic |

If `bridge` is chosen explicitly and `NO_COLOR` is set, bridge renders without escapes (the
glyphs still distinguish everything). Explicit choice beats the default; `NO_COLOR` only removes
color. A pending shot draws `?` in all three.

**R3. Where the choice lives: the seat's shell, not the disk.** Nothing is written to disk for
this. Resolution order, first hit wins:
1. `--format <id>` on `play` / `register` (before the moves: `zs play --format bridge B5`),
2. env `ZEROSUM_FORMAT`,
3. the game's default rule (R2; Farkle and Liar's Dice: their one format, `default`).

`0sum register <SID> <name> --format ledger` validates the id against the game and bakes it into
the printed load hint: `read -rs ZP; zs() { ZEROSUM_PASSWORD="$ZP" ZEROSUM_FORMAT=ledger 0sum "$@"; }`
(without `--format`, the hint is unchanged byte for byte). Changeable later: `zs play --format
bridge` for one run, or redefine `zs`. Why not a prefs file: same-Unix-user threat model, the
other seat could edit it; and a file is state to keep, test and leak. A shell env is per terminal,
which is per seat in hotseat.

Matching (H7): the id is matched exactly against the game's `FORMATS` list (ASCII
case-insensitive, no trimming, no prefixes) and the matched `&'static str` from `FORMATS` is
what's baked, never the argv string. An unsupported id from `--format` is an error that prints
the given id through §3.4's escaping: `` battleship has no `foo` format (bridge, ledger, logbook) ``,
`` farkle has no `ledger` format (default) ``. From `ZEROSUM_FORMAT` it silently falls back to the
game's default (an LLM with `ZEROSUM_FORMAT=ledger` set globally can still play Farkle), and the
env value is never echoed. A flag is a request; the env is a preference.

**R4. `ledger`.** Plain text, no ANSI ever (even on a TTY), no grid, no JSON. One fact per line,
`label: value` with ` · ` separators. Squares as `A1`..`J10`. Ships in fleet order; square lists
row-major, space-separated. Ship names exactly as the fleet names, so they match `place` tokens.
A ship's squares as `A2-A6` (origin to end). Empty lists print `none`. Battle phase, from Alice's
seat:
```
turn 31 · you vs Bob · your shot
your fleet: carrier A2-A6 ok · battleship C9-F9 hit D9 E9 · cruiser E2-E4 ok · submarine H5-J5 ok · destroyer I8-I9 SUNK
enemy shots: misses A9 B3 B8 C5 D2 F6 G3 H1 H8 J1 J10
your shots: open hits B3 B4 · sunk cruiser D7-F7 · misses A1 C3 C7 D9 E5 E10 F2 G7 H4 I8
enemy afloat: carrier 5 · battleship 4 · submarine 3 · destroyer 2
you afloat: carrier 5 · battleship 4 · cruiser 3 · submarine 3
last: you F7 sunk cruiser · enemy E9 hit
next: zs play <square>, e.g. zs play B5
```
- `open hits` = my hits on squares not part of any sunk ship, anonymous (no ship guess), exactly
  as the grid. Each sunk ship is its own `sunk <ship> <span>` segment; none is `sunk none`.
- `enemy afloat` comes only from `fleets[opponent].sunk`: the fleet minus sunk ships, `name len`.
- `your fleet`: per ship `ok` / `hit <squares>` / `SUNK`; during placing a ship shows its span
  or `unplaced`.
- `enemy shots: misses …` is the opponent's misses on my board (hits on me are in `your fleet`).
- Pending: `last: you B5 pending`, and `waiting: enemy's answer to your shot at B5` replaces
  `next:`. Their turn: `next: waiting on enemy's shot`.
- The `e.g.` square is an unshot neighbour of the latest open hit, else the first unshot square.
- Placing:
```
placing · you vs Bob · enemy placed 3 of 5, not locked
your fleet: carrier A2-A6 · battleship C9-F9 · cruiser unplaced · submarine unplaced · destroyer unplaced
next: zs play place cruiser <square> <h|v>, or zs play place auto; then zs play ready
```
- Reveal/Over header lines: `over · you won: sank enemy's fleet; your fleet verified` /
  `over · enemy won: sank your fleet; enemy's fleet verified` / `over · enemy forfeits: <cheat
  words>` / `over · you forfeit: <cheat words>` / `reveal · you sank enemy's fleet · waiting on
  your final reveal` / `reveal · enemy sank your fleet · waiting on enemy's final reveal`. The
  fleet and shot lines follow, then `next:`.
- An `error:` line comes first when `View.error` is set.
- **Names (H6):** the opponent's name appears only in the header line (`… · you vs <name> · …`).
  Every other line uses the fixed word `enemy`. Names may contain spaces, letters and digits, so a
  seat named `you A1 hit` could otherwise forge words in body lines.

**R5. Interface.** The engine owns format id parsing and resolution (R3) and passes the resolved
`Format { id, color }` to `Game::render_as`; a game declares `const FORMATS` and
`default_format(tty, no_color)`. Farkle and Liar's Dice declare one format and render exactly as
before (byte-identical output; their tests prove it). They have no ledger.

**R6. Hint lines use the real syntax:** `zs play B5` / `zs play fire B5` /
`zs play place carrier A1 h` / `zs play place auto` / `zs play ready`. The mockups' `<cell>` and
`across|down` became `<square>` and `<h|v>`; nothing else about them changed.

---

## B§10. Engine fit: the complete list of engine changes

All of these are code-only. No byte on disk changes.

1. **`Game::may_move`** (default method). `apply_chain` calls it where it now matches on `to_move`:
   ```rust
   fn may_move(st: &Self::State, me: usize, names: &[String]) -> Result<(), String> {
       match Self::to_move(st) {
           Some(p) if p == me => Ok(()),
           Some(o) => Err(format!("it's {}'s turn, not yours", names[o])),
           None => Err(format!("waiting on {}", Self::waiting(st, names))),
       }
   }
   ```
   The default is today's logic verbatim, so Farkle and LD don't change. Battleship overrides it
   for the simultaneous Setup phase. `to_move` alone can only say "one seat", and placement is
   a *move* (a human choice) that both seats make at once. LD's simultaneous commits are duties,
   so it never needed this.
2. **`Game::seats() -> RangeInclusive<usize>`** (default `2..=MAX_SEATS`). `check_roster` takes
   the range. Its message is `need exactly <n> players` when the range is a single value, else
   `need <a> to <b> players`, which is byte-identical to today's for Farkle and LD.
   `new_session` and replay's genesis step pass `G::seats()`. `Driver::create` skips the count
   prompt when the range is a single value.
3. **`src/engine/slots.rs`**: B§2. Pure, vector-tested.
4. **Registry**: `"battleship" | "bs" | "battleships" => Engine::<Battleship>`, and the
   `unknown game` list gains `battleship`. The clap help for `New` and `Play` lists the game,
   its rulesets and moves.
5. **Render formats** (B§9.1 R5): `Game::FORMATS`, `default_format`, `render_as`, and the
   engine's `resolve_format`. Display only.
6. **Layout**: `src/battleship/{mod.rs, ruleset.rs, board.rs}`. `board.rs` holds the encodings,
   placement geometry (squares of a placement, bounds, overlap, 8-adjacency), read-back (B§3.2),
   auto placement (B§3.3) and legality (B§6.4). They are pure functions, shared by `plan`, `duty`
   and `fold`, so live play and verify run the same geometry. The looks and `view()` live in
   `src/battleship/draw.rs` (B§9.1), the tests in `src/battleship/tests.rs`.

   A placement (row, col, dir) of length n is on the board iff row < rows, col < cols, and
   (h: col + n ≤ cols; v: row + n ≤ rows), computed in u16. `board::squares(p, n) ->
   Option<Vec<u16>>` returns None for any placement not on the board and is the only code that
   turns a placement into squares; plan, read-back, auto, duty, fold and reveal all call it, and
   no index is computed before it.

**Tripwire statement:** no format change is needed and none is made. New event kinds live
inside `Body::Game`, which is already game-defined JSON. Existing sessions: same canonical
bytes, same hashes, same signatures, same messages.

---

## B§11. Reuse for a future deck (Hold'em): a note, not a design

`slots` is the generic piece: **one seat commits to an arrangement of `n` values, opens slots one
at a time in public, and finally reveals the seed so the whole array can be audited.** That's
Battleship exactly. For a deck it's `label = "holdem/deck"`, `n = 52`, `v_j = [card]`, and
`opens` is the per-card proof.

What Hold'em still needs on top, which is the part that makes it mental poker:
- **A joint shuffle nobody knows.** A single seat's committed deck is a deck its owner can see.
  Every seat must permute and mask in turn (commutative or re-randomizable encryption, e.g.
  ElGamal over Ristretto255 via `curve25519-dalek`). Here `slots` fits as each seat's
  *commitment to its permutation*, audited at hand's end.
- **Private openings.** A hole card must be opened to one seat only. That means per-seat key
  agreement (a new HKDF label for an X25519 key) and decryption shares only the recipient can
  combine. Public `opens` serves the board cards and the showdown.
- **Per-street participation.** Dealing the flop needs every seat's share, which is the
  co-signer problem of §9.6 again. `0sum watch` becomes close to a prerequisite.
- **Mucked hands.** Decide whether the end-of-hand audit forces folded hole cards open (it must,
  for full verification) or accepts unaudited mucks.

---

## B§12. What this does NOT protect against (in addition to §12)

1. **Mid-game legality.** Until a ship sinks, nothing proves it is a legal ship. A rigged fleet
   plays on until a sunk check, a counting rule or the final reveal catches it (B§4.4). It
   always gets caught before a win becomes final, but not before it has steered some play.
2. **Held-back sunk.** Not provable when it happens: on a hit that sinks, the defender can omit
   `sunk`, and the attacker keeps probing around a dead ship. It's proven later, at the late
   declaration (`SunkLate`), at 17 hits (`UnsunkAtFull`), or at the reveal (`Withheld`).
3. **Caught cheaters stall.** A liar whose next entry would prove the lie can stop answering or
   never reveal. The game waits forever with the stall attributed. A provisional winner who
   won't reveal never gets a final win, and the render says so.
4. **The probe harvest (the §12.2 class).** Bob snapshots the whole directory, fires at X, lets
   Alice's next run auto-answer X, then restores. One snapshot covers every turn until the
   restore, so it yields every square Alice answered in that window. If her answering run was a
   look, nothing vanishes from `your last move`; the only signs are a head lower than the last one
   she saw and Bob's shot at X gone from her board. The high-water mark stops every unplanned
   rollback; a planned whole-directory snapshot remains.
5. **Re-placing by rollback.** The seat that locks second can snapshot the whole directory just
   before its own `fleet`, play on, restore, and re-place. It keeps every square the first locker
   answered (her `fleet` is inside the snapshot, so her board is unchanged), while the squares it
   gave away describe a board that no longer exists. Same planned-snapshot class as B§12.4; the
   signs are a head that drops back to before `fleet` and a vanished `your last move: fired at …`.
6. **Cheater vs cheater.** The first lie proven, in log order, decides. There's no adjudication
   beyond that.
7. **Your own screen.** Your fleet is on your terminal and in your scrollback (§12.4). An agent
   player's transcript is on the shared disk and holds every render it saw, its own fleet
   included; `ledger` puts that fleet on one greppable line. This is §12.4: keep agent
   transcripts out of the opponent's reach, or accept it.
8. **Re-placed salt reuse.** Every commitment hides its value until its slot is opened. Opening
   `S_k` (a sunk, or the reveal) also opens every earlier `placed{k}`, so the opponent learns that
   ship's abandoned positions and, from log order, that ships placed while it sat there avoid
   those squares. Re-placing to the same position repeats the same bytes. Accepted: it excludes a
   few placements of unsunk ships.

---

## B§13. Crates

**None new.** `sha2 0.10` (slots), `hkdf 0.12` (`b` via `Secrets::derive`), `rand 0.8`
(`place auto` through `ctx.rng` only; it's never used in anything `fold` checks, so `gen_range`
stability doesn't matter), `serde`, `serde_json`, `hex`, `ed25519-dalek` as they are.
Not added: `chacha20poly1305` (B§3.2 makes it unnecessary), any Merkle or ZK crate (B§4.1).

---

## B§14. Tests and golden vectors

Derived from B§2 using the §13 passwords. Computed by an independent Python-stdlib
implementation written from this text (Appendix A; it also reproduces §13's `s[1]` and
`sign_seed` for p1, which shows that HKDF and `lp` match the Rust code). **The Rust build must
match these.** If it disagrees, the text arbitrates. Commit Appendix A as
`tests/vectors/bs_vectors.py` (a dev script CI doesn't run, like `ld_vectors.py`).

```
SID = TEST01, ruleset classic, fleet order carrier battleship cruiser submarine destroyer
p1 (Alice)  b  f2cf63d2c7bbaf44417ba15b2f8cdffa34a0f9e69f21bc68415e2f1a246f21f3
p2 (Bob)    b  4f471cbb4e6e65d9a9f67a08a6bc12b9624041c2981238fd84aa9e67854321e6

Alice fleet: carrier A1 h, battleship C1 h, cruiser E1 h, submarine G1 h, destroyer I1 h
  S[0..5]  4db202c95b85b97645ac7961b547635db0f5933210dc45cfd50ea69c2b433702
           f79dee13e5f6e640e64e2cf442661a85c2174448e2f02f48b65f90d3befcc182
           65d543f3b4dfe42e718f680701b70c71cbdc9caaf07111ca68d87bc036e852d4
           a6d8a42fe3a7c68946108b4772f45bb99379af57ce91b3d21358981badbb0285
           21ce6757bca95a2fc63f1f78d0f97eb7e03ec3fc49401b45d12ed97aafd292a5
  K[0]     4ca980dbe7ea55aadfafa83e3f82f5a03a6f158722680f12965600f90b9119ba   (A1, occ 1)
  K[1]     452b90ccb3b925d8a313e74743301f8e5ed5b18d195eed83ad471cefbfdc0862   (A2, occ 1)
  E5 (i=44, occ 0)  salt 1703a822b626e86667ad36986b87e639ee26c88049c4e9098d963c70c5c3629f
                    K    f218241ed851caea56d6c0d6f89eb389de30232968c0063f6c3e14a8688d3b99
  CELLDIGEST = SHA-256(K[0] ‖ … ‖ K[99])  058947ed5273739c5ccb405484c89c03ca1c977b2ae730c9e1711a77693c6545
  SHIPDIGEST = SHA-256(S[0] ‖ … ‖ S[4])   804d59ce645237414ca8821a3f0acd9f9523864c95da8ce9f2ed396defc79988

Bob fleet: carrier C3 v, battleship J7 h, cruiser A6 v, submarine E8 v, destroyer A1 h
  S[0..5]  ed6e74c4ca707440e2661f1c2a78eebf90af71b9f5833e2c3d075fa3e7897e68
           b12eaa5eb4018aaaa5009652cc80ee1f1e5a716825f811ee1a54f925192780bd
           e0e600130abc9f1d927711d462d72580c41aa1988a94cc187622e3aea1ea7968
           05205549d2c91bfe650be4260a894f4804090e7b11669b6c61d0953afdde58ec
           3146c13268d6418abbf8eba365e801de01198d5cb3e1001c9acc22531d94beff
  destroyer (k=4) salt  0b1a8baadc966c59769fd21f00db22bed833e3ca632f7b6c7b586c81870fc7b6
  A1 (i=0, occ 1)  salt 2611a88ad33250687eb28449f01ba06db6c72ad7d39515b6ce6eb0c8c3c464b9
                   K    335ec1682b9edcd101116472eadcddf0111aaa0fbeef74423a4085901a853861
  A2 (i=1, occ 1)  salt c07fedc4bb6638afbb0a2fbc177f4f72819718e9a68c75a0f2d348ed64840d05
                   K    fa676b548043c3f3590398929a2cd2684e8d3645b23c1f36c11a90b5dacea312
  E5 (i=44, occ 0) salt a392a1d935272d9a8a64a3683acc3058cf02155d38157239214f7acca049b1ee
                   K    5e0cc762201fe894db0f018573b17feca68c2b98ad4c4db05e67da4748cc9d7a
  CELLDIGEST  f30627da49421fb9072b5a2cf2c3bf9028be7c14500fc861d59acef811c3811c
  SHIPDIGEST  b880ef5f33a84f33f649a358bc3c49d748b6a5cf638460838c42c005051d5d27

Cheat vectors (each must NOT equal the honest commitment it targets):
  lie: Bob's A1 opened as water (salt as above, occ 0)   7b0b730ff794c977d8af8498b7f70fdcf937248e0f9b7f81e8a26b5f951de65e  ≠ K[0]
  move: Bob's destroyer opened as A1 v (salt as above)    668220fe6df11dfaa6a250745b1d7c2d7f3c91051c0a5b3aec97202fc87360f6  ≠ S[4]
  replay across seats: Bob's A1 opening under seat p1     ee2116377bce3465b6286fc140d7677e6756ffdff35ed7a0b1e59c6f7d9447f3  ≠ K[0]
  replay across sessions: same opening under SID TEST02   8e114d210e57b7a8fa788ed1dec2d36c005181aed05fa9f69ec07e991be8c8bf  ≠ K[0]
```

`CELLDIGEST`/`SHIPDIGEST` are test digests (one vector for a whole array), not protocol values.

Required tests (beyond the vectors):
- **Slots:** `opens` round-trips, and rejects a flipped value, a wrong index, a wrong label, a
  wrong seat and a wrong SID. Salts are pairwise distinct across all 105 slots of a board.
- **Read-back:** after any placement sequence (including re-placing a ship), reading back with
  the owner's `Secrets` returns exactly the placements. With another seat's `Secrets` it fails.
- **A full game** (B§7.3 fleets, scripted shots through `apply_chain` and `replay`): reaches
  Over with `How::Sank`. The final `replay` equals the live state. `verify` narrates it and ends `OK`.
  Every answer was produced by `duty`, never by hand.
- **Placement rules:** off-board, overlap, `notouch` adjacency (diagonal included), the ambiguous
  `c`, re-place before `ready`, `ready` with a ship missing, and `place auto` (seeded) yielding a
  legal fleet under both rulesets. `place auto` with all placed re-rolls.
- **Shots:** out of turn, double fire before an answer, an off-board square, a repeated square,
  and `fire` during Setup. Each gets the B§8.2 message. `place` after `ready` too.
- **2 players:** `new` asks no count. A 3-seat battleship genesis → `BROKEN at seq 0: need exactly 2 players`.
- **Cheating, one per vector** (signed with the cheater's real key, so only the protocol can catch them):
  - *Move a ship after commit:* (a) a `placed` after `fleet` → BROKEN `p2 moved a ship after
    locking the fleet`; (b) a sunk opening with the destroyer at A1 v → BROKEN `doesn't open`
    (move vector); (c) a reveal listing a moved ship → BROKEN `revealed <ship> doesn't open`.
  - *Lie about hit/miss:* an answer `{0, hit:false, true salt}` → BROKEN `p2's answer for A1
    doesn't open…` (lie vector). Also: a *board* rigged at commit (one carrier square committed
    as water, built by a test helper that bypasses `plan`) → Alice can't pass 16 hits, and at
    84 misses → Forfeit `TooManyMisses`; if Bob "wins" first, his reveal → Forfeit `Hidden`.
  - *Lie about sunk:* sunk declared with a square unhit → Forfeit `SunkEarly`. Sunk omitted on the
    completing hit, then declared later → Forfeit `SunkLate`. Omitted for good → Forfeit at 17
    hits (`UnsunkAtFull`) or at reveal (`Withheld`). A sunk on a miss → Forfeit `SunkEarly` or
    `SunkLate`. Double sunk → BROKEN.
  - A sunk or reveal with the carrier at A8 h (row 0, col 7, h) → Forfeit `OffBoard`; with row
    255 → Forfeit `OffBoard`, no panic.
  - *Replay a commitment:* Bob's `placed`/`fleet` copied from Alice's entries (re-signed by Bob)
    fold fine, but no answer Bob can construct opens them (the seat vector), so the honest
    `duty` returns None and the game waits on Bob. Openings across SID → no match (the session vector).
  - *Read the other board from disk:* after Setup and a few shots, the session file and every
    file under the store contain neither seat's `b`, nor any salt of an **unshot** square or
    unsunk ship (grep for each derived hex). The only plaintext board facts on disk are opened
    squares and sunk ships.
- **Forfeit plumbing:** a forfeit ends the game at once (`Over`, the right winner, no reveals
  owed). Any later entry is BROKEN `the game is over`. `verify` prints the `CHEAT:` line and ends `OK`.
- **Reveal:** a wrong `b` → BROKEN `board secret doesn't open…`. A legal reveal → `Sank`. The
  provisional winner's illegal fleet → the loser wins by forfeit. The loser's reveal → BROKEN
  `owes no reveal`.
- **Render formats:** each format on the same mid-game `View` (the mockup position, matched
  against `tests/looks/`); the ledger's `open hits` line; no ANSI in ledger or logbook; an
  unsupported `--format` errors, an unsupported env value falls back silently; the opponent's
  name only on the ledger's header line.
- **Invariants:** existing Farkle and LD suites green, untouched. A saved LD session fixture and
  the §13 vectors verify byte-identically. `check_roster` still says `need 2 to 8 players` for them.

---

## B§15. Build order and out of scope

One PR, in commits that each stay green:
1. The engine seam: `may_move`, `seats()`, `slots.rs` with its vectors. Farkle and LD untouched and green.
2. `battleship/{ruleset,board}`: geometry, read-back, auto placement, legality, pure-fn tests.
3. `battleship/mod.rs`: events, `fold`, `duty`, `plan`, `view()` and the looks. Registry.
4. The full-game, cheating, verify and storage tests above.
5. The three looks of B§9.1 (kautau picked bridge). README section (Paige).

Out of scope, deliberately: `salvo` (B§5), ZK legality (B§4.1), `0sum watch`, Hold'em (B§11),
and any change to LD's `dice.rs`.

---

## Appendix A: `tests/vectors/bs_vectors.py`

```python
#!/usr/bin/env python3
"""Battleship golden vectors (docs/BATTLESHIP.md B§14), stdlib only, written from the spec."""
import hashlib, hmac, struct

H = lambda *p: hashlib.sha256(b"".join(p)).digest()
lp = lambda x: struct.pack(">H", len(x)) + x
u16 = lambda j: struct.pack(">H", j)

def prk(pw):
    sid, sec = pw.split("-")
    return hmac.new(("0sum/v2/" + sid).encode(), sec.encode(), hashlib.sha256).digest()

def expand(p, info):  # one 32-byte HKDF-SHA256 block
    return hmac.new(p, info + b"\x01", hashlib.sha256).digest()

def salt(seed, label, j):
    return H(b"0sum/v2/slot-salt\0", lp(label.encode()), seed, u16(j))

def slot(label, sid, seat, j, s, v):
    return H(b"0sum/v2/slot\0", lp(label.encode()), lp(sid.encode()), lp(seat.encode()), u16(j), s, lp(bytes(v)))

FLEET = [5, 4, 3, 3, 2]
def occupancy(pl):
    occ = [0] * 100
    for k, (r, c, d) in enumerate(pl):
        assert (c + FLEET[k] <= 10) if d == 0 else (r + FLEET[k] <= 10)
        for t in range(FLEET[k]):
            i = (r * 10 + c + t) if d == 0 else ((r + t) * 10 + c)
            assert occ[i] == 0
            occ[i] = 1
    return occ

P1, P2 = "TEST01-AAAAAAAAAAAAAAAAAAAAAAAAAA", "TEST01-BBBBBBBBBBBBBBBBBBBBBBBBBB"
assert expand(prk(P1), b"0sum/v2/sign").hex().startswith("06e3cefdb9a2f24c")       # PHASE2 §13
assert expand(prk(P1), b"0sum/v2/liars-dice/secret" + struct.pack(">I", 1)).hex().startswith("612f661662e64504")

ALICE = [(0, 0, 0), (2, 0, 0), (4, 0, 0), (6, 0, 0), (8, 0, 0)]   # (row, col, dir) dir 0=h 1=v
BOB = [(2, 2, 1), (9, 6, 0), (0, 5, 1), (4, 7, 1), (0, 0, 0)]
for who, pw, seat, pl in [("alice", P1, "p1", ALICE), ("bob", P2, "p2", BOB)]:
    b = expand(prk(pw), b"0sum/v2/battleship/board")
    occ = occupancy(pl)
    S = [slot("battleship/ship", "TEST01", seat, k, salt(b, "battleship/ship", k), pl[k]) for k in range(5)]
    K = [slot("battleship/cell", "TEST01", seat, i, salt(b, "battleship/cell", i), [occ[i]]) for i in range(100)]
    print(who, "b", b.hex())
    for k in range(5): print(who, f"S[{k}]", S[k].hex())
    for i in (0, 1, 44): print(who, f"cell {i} occ {occ[i]} salt", salt(b, "battleship/cell", i).hex(), "K", K[i].hex())
    print(who, "ship 4 salt", salt(b, "battleship/ship", 4).hex())
    print(who, "CELLDIGEST", H(*K).hex(), "SHIPDIGEST", H(*S).hex())
    if who == "bob":
        s0, s4 = salt(b, "battleship/cell", 0), salt(b, "battleship/ship", 4)
        print("lie A1 as water", slot("battleship/cell", "TEST01", "p2", 0, s0, [0]).hex())
        print("move destroyer A1 v", slot("battleship/ship", "TEST01", "p2", 4, s4, [0, 0, 1]).hex())
        print("replay seat p1", slot("battleship/cell", "TEST01", "p1", 0, s0, [1]).hex())
        print("replay SID TEST02", slot("battleship/cell", "TEST02", "p2", 0, s0, [1]).hex())
```

---

## Appendix B: notes from the build

Choices the text above left open, or settled between two of its sections:

- `Cheat::OffBoard` carries the placement (`OffBoard { ship, at }`) so every wording can say
  where the ship was put. `Cheat` lives in memory only, so this changes no byte on disk.
- The reveal checks the cell slots before the ship slots: a wrong `b` then reports `board secret
  doesn't open…` (B§14), and a moved ship under the right `b` reports `revealed <ship> doesn't
  open…`.
- `Board.revealed` isn't stored: since A2 it equals `phase == Over` with `How::Sank`.
- `p1 already revealed` is unreachable since A2: a reveal either forfeits or ends the game, so a
  second one meets `the game is over`. It isn't in the code.
- A repeated shot is refused by `plan` as `you already fired at B5` (B§7.1); a signed one that gets
  past `plan` meets `fold`'s `p1 already fired at B5` (B§8.2).
- The answer duty takes `hit` as whichever value opens the seat's own committed square, rather
  than recomputing it from the read-back fleet. For an honest board the two agree; for any other
  the duty still never hands `fold` an answer it rejects.
- `new` for a fixed-size game prints `Battleship is a 2-player game.` from the first word of
  `describe`, not from a new trait item.

