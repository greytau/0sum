# 0sum

**Zero-sum CLI games you play against each other — every move signed, every hidden die and every hit provable.**

`0sum` is a Rust command-line tool for playing turn-based games against other
people at the same machine, built so that cheating is *hard and leaves evidence*.
Fairness is checked with cryptography rather than taken on trust. Every move is signed, the
session is a tamper-evident hash-chained log, and the game state is replayed from that
log, so nobody can quietly edit history.

Games: **Farkle** and **Liar's Dice** (dice), and **Battleship** (hidden boards).
Cards (poker) come later, once the crypto to hide a shared deck from every player
is earned.

The christening project of the [`greytau`](https://github.com/greytau) org.

---

## Status

**Phase 2 (signed log, verify, Liar's Dice), plus Battleship.**

- Every session is one JSON file holding an append-only log. Each entry is
  SHA-256 hash-chained to the one before it and Ed25519-signed by its author.
  Every command replays and checks the whole log before acting, and refuses to
  add to a broken one.
- `0sum verify <SESSION>` needs no password. It replays the log, prints what
  happened, and ends with `OK …` or `BROKEN at seq N: …`.
- **Liar's Dice has fair hidden dice.** Each player commits to a secret before
  anyone publishes entropy, so nobody (the roller included) can choose or peek at
  dice. When someone calls, every player's secret is revealed, and anyone can
  re-derive every die and check the count.
- **Battleship has hidden boards that can't be moved, and hits that can't be
  faked.** Each player
  commits to their fleet, and the log holds only those commitments, so reading
  the disk tells the other player nothing. After `ready` a ship can't move. Every
  hit or miss is answered by opening that square's commitment, and the opening is
  checked the moment it lands; a "sunk" also opens the ship, proving its name and
  position.
- Passwords are minted by the binary, never written to disk, and never typed on
  the command line. Nothing secret is stored: your dice and your fleet are
  recomputed from your password and the log each time you look.
- Each player's binary keeps a signed high-water mark of the last log it saw,
  and refuses a log that was rolled back past it.

**Farkle rolls are still trusted to the roller:** whoever rolls signs the dice,
so a player who edits the file can pick their own rolls (fixing that needs an
online co-signer; see [`docs/PHASE2.md`](docs/PHASE2.md) §9.6). **Battleship can't
prove mid-game that a fleet is legal** (five straight ships of the right sizes),
or that a defender didn't hold back a "sunk". Both get caught later: at a sunk, by
counting (17 hits must mean five ships sunk), or when the winner reveals their
board at the end. A caught cheater forfeits, and the honest player's win is in the
log. A cheater can still just stop playing, which stalls the game without giving
them a win. Players on one machine and one account can also defeat some
protections, for example by snapshotting the whole directory before a rollback.
§12 of the Phase 2 spec and B§12 of the Battleship spec list every known limit,
plainly.

Farkle's default ruleset is `kcd` (Kingdom Come: Deliverance scoring, first to
4000; the Devil's head wild die is not implemented). `--ruleset classic` plays to
10000. Liar's Dice defaults to `classic` (5 dice each, 1s wild); `--ruleset natural`
makes 1s an ordinary face and allows `spot`. Battleship defaults to `classic`
(10×10; carrier 5, battleship 4, cruiser 3, submarine 3, destroyer 2; ships may
touch); `--ruleset notouch` keeps ships apart, diagonals included.

## Building

```console
cargo install --path .   # builds 0sum and puts it on your PATH (~/.cargo/bin)
0sum new ld              # start a game (see "Play Liar's Dice in two terminals")
```

Requires Rust 1.89 or newer. `cargo build` alone leaves the binary at
`target/debug/0sum`, which isn't on your `PATH`; the commands below assume `0sum` is.

## Your password, once per terminal

`register` prints your password once. Load it into a shell variable that is
**not** exported, plus a tiny `zs` helper that hands it to `0sum` alone:

```console
$ read -rs ZP; zs() { ZEROSUM_PASSWORD="$ZP" 0sum "$@"; }   # paste the password, Enter
$ zs play                                                   # look at the table
```

Never put the password on the command line (it would land in your shell
history), and don't `export` it (every program you start would inherit it).

## Play Liar's Dice in two terminals

**Anywhere:** create the session. It asks how many players (2 to 8) and their names.

```console
$ 0sum new liars-dice          # or: 0sum new ld
How many players? [2]: 2
Player 1 name: Alice
Player 2 name: Bob
  SESSION ID:  BK3DFX
```

**Terminal A (Alice) and terminal B (Bob):** each claims a seat, then loads the
password it prints:

```console
A$ 0sum register BK3DFX Alice
A$ read -rs ZP; zs() { ZEROSUM_PASSWORD="$ZP" 0sum "$@"; }

B$ 0sum register BK3DFX Bob
B$ read -rs ZP; zs() { ZEROSUM_PASSWORD="$ZP" 0sum "$@"; }
```

**Then loop: look, then act.** Committing, rolling and revealing happen
automatically every time you run `zs play`, so there is no `roll` command. Each
player's first look rolls their dice, and you see yours once everyone else has
rolled, so round 1 starts with a look from each of you:

```console
A$ zs play      # Alice commits and rolls; waiting on Bob's roll
B$ zs play      # Bob rolls, and now sees his dice
A$ zs play      # Alice sees hers; her turn to bid
```

`zs play` with no move shows the table and your dice. Here is Bob's second look:

```console
B$ zs play
--- Liar's Dice, ruleset classic (5 dice each, 1s wild) · round 1 ---
  * Alice  5 dice
    Bob    5 dice
  your dice: 4 4 4 5 6

Alice's turn to bid

your last move: registered (seq 2)
head 6:78d811f01b8c
```

On your turn, bid a quantity and a face ("at least two 4s on the table"), or
call the last bid a lie:

```console
A$ zs play bid 2 4             # or: zs play b 2 4
B$ zs play bid 4 4
A$ zs play liar                # or: zs play l / call
```

1s are wild and count toward every face, so you bid faces 2 to 6. A new bid
must raise the quantity, or keep it and raise the face.

**When a round resolves,** the next `zs play` by each player shows every hand
and the count. The loser drops a die and starts the next round:

```console
Round 1 result: Alice called liar on Bob's 4×4.
  Alice  1 5 5 6 6
  Bob    4 4 4 5 6
  4s showing: 4 (1s wild) — the bid holds. Alice loses a die (4 left).
```

Each run also prints `your last move: …` and `head <seq>:<hash>`. If a move of
yours disappears or the head goes backwards, someone rolled the file back. Lose
your last die and you're out; the last player holding dice wins.

**Check every reveal,** during or after the game, with no password:

```console
$ 0sum verify BK3DFX
  10  Alice reveals: 1 5 5 6 6
  12  Bob reveals: 4 4 4 5 6
  12  Round 1 result: Alice called liar on Bob's 4×4.
  ...
OK  56 entries  head 55:5b170b4a930c
```

`verify` checks each revealed secret against its commitment, re-derives every
die, recounts every call, and re-checks every signature.

## Play Battleship in two terminals

**Anywhere:** create the session. Battleship is always two players, so it only
asks for names.

```console
$ 0sum new battleship          # or: 0sum new bs   (add --ruleset notouch to keep ships apart)
Battleship is a 2-player game.
Player 1 name: Alice
Player 2 name: Bob
Created Battleship, ruleset classic (10×10, fleet 5 4 3 3 2)
  SESSION ID:  9GSHYF
```

**Terminal A (Alice) and terminal B (Bob):** each claims a seat and loads the
password, exactly as above. `register` also takes `--format` to pick how this
seat's board is drawn (see [Display formats](#display-formats)). The format
isn't stored anywhere: `register` bakes it into the `zs` line it prints for you
to run. Here Alice takes the default and Bob asks for `ledger`:

```console
A$ 0sum register 9GSHYF Alice
...
Load it into this terminal: run this, paste the password at the silent prompt, Enter.
  read -rs ZP; zs() { ZEROSUM_PASSWORD="$ZP" 0sum "$@"; }

B$ 0sum register 9GSHYF Bob --format ledger
...
Load it into this terminal: run this, paste the password at the silent prompt, Enter.
  read -rs ZP; zs() { ZEROSUM_PASSWORD="$ZP" ZEROSUM_FORMAT=ledger 0sum "$@"; }
```

Each of you runs the `zs` line your own `register` printed, then pastes the password.

**Place your fleet** once both seats are registered. Squares are a row letter and
a column number, `A1` to `J10`. A ship goes at its top or left end, then `h`
(across) or `v` (down). Ship names can be shortened to any unique start: `ca` is
the carrier, `cr` the cruiser, `b`, `s` and `d` the rest (plain `c` is ambiguous,
and `0sum` will say so). One `place` can take several ships:

```console
A$ zs play place carrier A1 h battleship C1 h
A$ zs play place battleship E1 v       # changed your mind? place it again, free until ready
A$ zs play place auto                  # put the rest down at random
A$ zs play ready                       # lock the fleet (or: lock, done)
```

`place auto` fills every ship you haven't placed yet. If they're all placed, it
re-rolls the whole fleet, so you can run it until you like the layout. Moves
chain, so the quick way is one command:

```console
B$ zs play place auto ready
Bob placed the carrier
...
Bob locked the fleet
Both fleets locked. Alice fires first.
```

`ready` is the lock. After it, nobody can move a ship, and your binary will
refuse to place one.

**Fire** with `zs play` and a square (or `zs play fire B5`; `f` and `shoot` work
too). One shot per turn, and a hit doesn't earn another. Your opponent's binary
answers your shot the next time they run `0sum`, whatever they type, so
answering needs no command and can't be skipped by an honest binary.

**Each turn is one look and one shot.** The look shows the answer to your last
shot (and answers theirs), then you aim. Here is Alice's first shot, drawn in
`bridge`, the default in a color terminal (`?` is a shot still waiting for its
answer):

```console
A$ zs play            # look (output not shown)
A$ zs play B5
Alice fires at B5
╭─ 0sum · battleship ──────────────────────────────── Alice vs Bob · shot 1 ─╮
│ YOUR FLEET                           YOUR SHOTS                            │
│    1 2 3 4 5 6 7 8 9 10                 1 2 3 4 5 6 7 8 9 10               │
│ A  ■ ■ ■ ■ ■ · · · · ·               A  · · · · · · · · · ·                │
│ B  · · · · · · · · · ·               B  · · · · ? · · · · ·                │
│ C  · · · · · · ■ · · ·               C  · · · · · · · · · ·                │
│ D  · · · · · ■ ■ · · ·               D  · · · · · · · · · ·                │
│ E  ■ · · ■ · ■ · · · ·               E  · · · · · · · · · ·                │
│ F  ■ · · ■ · ■ · · · ·               F  · · · · · · · · · ·                │
│ G  ■ · · ■ · · · · · ·               G  · · · · · · · · · ·                │
│ H  ■ · · · · · · · · ·               H  · · · · · · · · · ·                │
│ I  · · · · · · · · · ·               I  · · · · · · · · · ·                │
│ J  · · · · · · · · · ·               J  · · · · · · · · · ·                │
│                                                                            │
│ ■■■■■ carrier                        ■■■■■ carrier                         │
│ ■■■■  battleship                     ■■■■  battleship                      │
│ ■■■   cruiser                        ■■■   cruiser                         │
│ ■■■   submarine                      ■■■   submarine                       │
│ ■■    destroyer                      ■■    destroyer                       │
├────────────────────────────────────────────────────────────────────────────┤
│ ▸ you fired B5 — waiting on Bob      ◂ Bob hasn't fired yet                │
│ · water  ■ ship  ✕ hit  ▓ sunk  ○ miss        afloat  you 5/5 · Bob 5/5    │
╰─ waiting ▶ Bob's answer to your shot at B5 ────────────────────────────────╯
your last move: fired at B5 (shot 1, seq 16)
head 16:7efd0398dda7
```

And Bob's turn, in `ledger`. His look answers Alice's shot; then he fires:

```console
B$ zs play
turn 2 · you vs Alice · your shot
your fleet: carrier E2-I2 ok · battleship D1-D4 ok · cruiser G4-G6 ok · submarine C4-C6 ok · destroyer A4-A5 ok
enemy shots: misses B5
your shots: open hits none · sunk none · misses none
enemy afloat: carrier 5 · battleship 4 · cruiser 3 · submarine 3 · destroyer 2
you afloat: carrier 5 · battleship 4 · cruiser 3 · submarine 3 · destroyer 2
last: you none · enemy B5 miss
next: zs play <square>, e.g. zs play A1
...
B$ zs play E5
turn 2 · you vs Alice · waiting on enemy's answer
...
```

You can skip the look: `zs play E5` on its own answers first, then fires. The
look is just how you see your last result before you aim.

A hit tells you only that the square is occupied, not which ship, just like the
board game. The shot that completes a ship also opens that ship, so "you sank my
destroyer" comes with proof, and your shots grid draws its shape.

**How it ends.** The first player to land 17 hits, with all five ships declared
sunk, is the *provisional* winner. Their next run reveals their own board, which
opens every commitment they made, and `0sum` checks that their fleet was legal and
that every answer they gave matched it. Only then is the win final:

```console
B$ zs play --format logbook        # ledger would show only the look
Bob reveals: carrier E2 v, battleship D1 h, cruiser G4 h, submarine C4 h, destroyer A4 h — legal, every answer matched
Bob won: sank Alice's fleet; Bob's fleet verified
```

Until that run, the other side shows
`Bob sank Alice's fleet; waiting on Bob's final reveal to confirm`. The loser
reveals nothing extra: their sunk ships already showed their whole fleet.

If an answer ever proves its author lied (a "sunk" that comes too late, 17 hits
but not five sinkings, a revealed fleet that's illegal or doesn't match its
answers), the game ends there: the liar forfeits and the other player wins,
recorded in the log. An entry that doesn't even open its commitment is a broken
log, not a move, and no honest binary will add to it.

**Check the whole game,** during or after, with no password:

```console
$ 0sum verify 9GSHYF
   0  genesis: Battleship, ruleset classic (10×10, fleet 5 4 3 3 2); seats: p1 Alice, p2 Bob
   1  Alice registered (key 3b58f65055a5)
   2  Bob registered (key ed1bd1dfa4ec)
   3  Alice placed the carrier
  ...
  16  Alice fires at B5
  17  Bob: B5 — miss
  ...
 243  Bob sank Alice's fleet; waiting on Bob's final reveal to confirm
 244  Bob reveals: carrier E2 v, battleship D1 h, cruiser G4 h, submarine C4 h, destroyer A4 h — legal, every answer matched
 244  Bob won: sank Alice's fleet; Bob's fleet verified
OK  245 entries  head 244:4c04abb2fbf7
```

Mid-game, `verify` already proves every hit, miss and sunk so far against its
commitment. What it can't prove yet is that the fleets are legal and that no
"sunk" was held back; those are settled by later sinkings, by counting, and by
the winner's reveal. Run it at the end to see the whole game proved.

## Display formats

Battleship can draw your board three ways. Farkle and Liar's Dice have one look
each, so none of this changes them.

| Format | What it looks like | Default when |
|---|---|---|
| `bridge` | rounded box, `· ■ ✕ ▓ ○` glyphs, ship roster with damage, color | stdout is a terminal and `NO_COLOR` is unset |
| `logbook` | plain ASCII grid, `. # X = o`, no escape codes | output is piped, or `NO_COLOR` is set |
| `ledger` | one fact per line, squares as `A1`…`J10`, no grid | never; you ask for it |

**`ledger` is for players who read text line by line, like LLM agents.**
It prints no narration (no `Bob: B5 — miss`, no `Alice fires at …`, no
`… won: …` line): the facts are all in the look, and the opponent's name is
only ever on its header line.

Pick a format for one run with `--format`, before or after the moves (once per run):

```console
A$ zs play --format logbook
A$ zs play --format ledger B5
A$ zs play B5 --format ledger
```

To change it for every
run in a terminal, `export ZEROSUM_FORMAT=ledger` there (it isn't a secret), or
use the `zs` line `register --format` printed. `--format` beats `ZEROSUM_FORMAT`, which beats the
default. Asking for `bridge` with `NO_COLOR` set gives you bridge without the
color. A format your game doesn't have is an error with `--format`
(`` farkle has no `ledger` format (default) ``), and quietly ignored in
`ZEROSUM_FORMAT`, so an agent with `ZEROSUM_FORMAT=ledger` set can still play
Farkle.

Formats only change what you see, never what's signed, and no format shows
more than your seat is allowed to know. One caution for agent players:
`ledger` puts your whole fleet on one line of text, so keep the agent's
transcript somewhere your opponent can't read it.

## Farkle

```console
$ 0sum new farkle                 # prompts for players, prints a SESSION ID
$ 0sum register <SESSION> <name>  # claim your seat, load the password as above
$ zs play roll
$ zs play keep 1 5                # keep scoring dice you rolled (here, a 1 and a 5)
$ zs play bank                    # end your turn and keep the points
$ zs play keep 1 5 bank           # moves chain in one call; r / k / b also work
```

Chained moves apply in order and stop at the first one that ends your turn (bank,
bust, or a win) or is illegal; the moves before it still count.

## Design

The Phase 2 spec (the signed log, the fair hidden-dice protocol, the engine, Liar's
Dice, and what none of it protects against) is [`docs/PHASE2.md`](docs/PHASE2.md).
The Battleship spec (the fleet commitment, provable answers, forfeit versus broken,
the display formats, and its own list of limits) is
[`docs/BATTLESHIP.md`](docs/BATTLESHIP.md).
The original design brief (CLI flow, the three-tier hidden-state format, phased
fairness, the stakes/veto layer, the build phases) is
[`docs/DESIGN.md`](docs/DESIGN.md), tracked canonically in
[issue #1](https://github.com/greytau/0sum/issues/1).

## License

Dual-licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option. Unless you explicitly state otherwise, any contribution
intentionally submitted for inclusion in this project by you, as defined in the
Apache-2.0 license, shall be dual-licensed as above, without any additional
terms or conditions.

---

<sub>Package name on crates.io: `zerosum` (Cargo won't take a name with a leading
digit — the binary is still `0sum`). Fine print: `0sum` quietly stands for
**P**rotected **R**andomized **E**ncrypted **Ga**ming — *PREGante*. That is not
its real name. Its real name is `0sum`. You didn't read this.</sub>
