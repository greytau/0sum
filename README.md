# 0sum

**Zero-sum CLI games you play against each other — every move signed, every hidden die provable.**

`0sum` is a Rust command-line tool for playing turn-based games against other
people at the same machine, built so that cheating is *hard and leaves evidence*.
Fairness is checked with cryptography rather than taken on trust. Every move is signed, the
session is a tamper-evident hash-chained log, and the game state is replayed from that
log, so nobody can quietly edit history.

Games: **Farkle** and **Liar's Dice** (dice). Cards (poker) come later, once the
crypto to hide a shared deck from every player is earned.

The christening project of the [`greytau`](https://github.com/greytau) org.

---

## Status

**Phase 2: signed log, verify, Liar's Dice.**

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
- Passwords are minted by the binary, never written to disk, and never typed on
  the command line. Nothing secret is stored: your dice are recomputed from your
  password each time you look.
- Each player's binary keeps a signed high-water mark of the last log it saw,
  and refuses a log that was rolled back past it.

**Farkle rolls are still trusted to the roller:** whoever rolls signs the dice,
so a player who edits the file can pick their own rolls (fixing that needs an
online co-signer; see [`docs/PHASE2.md`](docs/PHASE2.md) §9.6). Players on one
machine and one account can also defeat some protections, for example by
snapshotting the whole directory before a rollback. §12 of the spec lists every
known limit, plainly.

Farkle's default ruleset is `kcd` (Kingdom Come: Deliverance scoring, first to
4000; the Devil's head wild die is not implemented). `--ruleset classic` plays to
10000. Liar's Dice defaults to `classic` (5 dice each, 1s wild); `--ruleset natural`
makes 1s an ordinary face and allows `spot`.

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
