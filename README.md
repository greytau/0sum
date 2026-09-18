# 0sum

**Provably-fair CLI games you play against each other — zero-sum.**

`0sum` is a Rust command-line tool for playing turn-based games against another
person, where *neither player can secretly cheat*. Fairness is not a promise —
it's cryptographic. Every move is signed, every game state is a tamper-evident
hash-chained log, and the final score is a projection over history that no one
can quietly edit.

First game: **Farkle** (dice). Cards (poker) come later, once the crypto to hide
a shared deck from both players is earned.

The christening project of the [`greytau`](https://github.com/greytau) org.

---

## Status

**Phase 1 — hotseat Farkle is playable.** The engine, the `Game` trait, rulesets,
and the CLI flow are in. Plain-text UI. The default ruleset is `kcd` (Kingdom Come:
Deliverance scoring, first to 4000; the Devil's head wild die is not implemented).

Phase 1 is *hotseat*: dice come from the local machine's OS entropy, so it is fair
when both players share a terminal. Cross-machine play needs the commit-reveal dice
described in the design — that is the next phase, not this one.

The command builds to a binary named `0sum`:

```console
$ 0sum new farkle                 # prompts for players, prints a SESSION ID
$ 0sum register <SESSION> <name>  # claim your seat, prints your password (keep it secret)
$ 0sum play <password> roll
$ 0sum play <password> keep 1 5   # set aside scoring dice from the table
$ 0sum play <password> bank       # end your turn and keep the points
```

## Design

The full design brief — CLI flow, the signed-hash-chain state model, the
three-tier hidden-state format, phased fairness (commit-reveal dice → mental-poker
cards), the stakes/veto layer, and the build phases — lives in
[`docs/DESIGN.md`](docs/DESIGN.md) and is tracked canonically in
[issue #1](https://github.com/greytau/0sum/issues/1).

## Building

```console
cargo build          # produces target/debug/0sum
cargo run            # prints the WIP banner
```

Requires a recent stable Rust toolchain.

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
