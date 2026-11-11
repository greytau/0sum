> **Phase 2 (signed log, fair hidden dice, engine, Liar's Dice) is specified in [`PHASE2.md`](PHASE2.md); it supersedes the crypto sketched here.**

# 0sum — CLI Games Binary (design doc)

> Design brief, captured Wake 420 (2026-07-13). Tracked canonically in
> [issue #1](https://github.com/greytau/0sum/issues/1); this file is the
> in-repo copy.

**Org:** `greytau` (GitHub org — name = *grey* [Poupon] + *tau* [the golden ratio
in kautau, pairs with Q's phi]).
**Command / binary:** `0sum` — invoked as `0sum new farkle`, etc.
**Cargo crate:** `zerosum` (Cargo won't accept a package name with a leading
digit; the `[[bin]]` target is still named `0sum`, so the built executable is
literally `0sum`).

---

## Vision

A Rust CLI for two people (kautau + Q; extensible to more) to play **turn-based
games against each other with provable fairness.** First game: **Farkle** (dice).
Later: poker (cards). The christening project of the `greytau` org, and the "play
games across the silicon bridge" idea made real.

Lineage in memory: two raw Anytype holons from Wake 399 (card-dealer core
`kv-59TjPS` + banker/escrow/veto enrichments `kv-3tjrjd`) — the *fairness
philosophy* is captured there; everything below about the Rust binary /
Farkle-first / CLI flow / greytau is **new tonight** and lives here.

---

## Decisions locked (Wake 420)

- **Name decided.** Command `0sum`, crate `zerosum`, org `greytau`. (Was the
  standing open question — now closed.)
- **Hotseat first.** Remote (Matrix async) is phase 2.
- **Farkle first** — a *conscious reversal* of the original captured "cards-first"
  ordering. Reason: dice are **all-public** (cheap commit-reveal fairness), cards
  have **hidden shared state** (heavy mental-poker crypto). Farkle lets us build
  the whole skeleton against the cheap primitive, then earn the hard crypto for
  cards.
- **Stateless CLI invocations** over **one self-contained state file**
  (git-meets-blockchain).

---

## CLI flow

```
0sum new farkle             # prompts: number of players / names
                            # → returns a short base32 SESSION ID (public, human-writable)

0sum register <session-id> <name>
                            # → returns a private PASSWORD, per player (kept secret)

0sum play <password> <moves>
                            # password alone identifies (session + player); no session-id re-type
```

- **Session id** = public room code, short, easy to copy/paste/write down.
- **Password** = private per-player secret (see State model — it *is* a signing
  key). Longer (a real key); copy/paste, not hand-written.
- **Local index** `~/.0sum/` maps password-hash → session file, so
  `play <password>` needs no session id. High-entropy passwords → globally unique
  → no collision.

---

## State model — the tamper-evident core (phase 1, Farkle)

The state file is an **append-only, hash-chained, per-move-signed log.** Three
properties:

1. **Never store the score — store the *moves*, compute the score.** Score is a
   *projection* (fold over the log). No `score:` field to edit; faking a score
   means faking *history*.
2. **Hash-chain every entry** (git/blockchain style): each move embeds the hash of
   the previous. Any silent edit to a past move breaks every subsequent hash →
   detectable.
3. **The password IS an Ed25519 signing key.** At `register`, generate a keypair;
   the private key is the password handed to the player, the public key goes in
   the session genesis block. Every `play` move is signed. To rewrite the chain
   you'd have to re-sign every later move with keys **you don't have** →
   cryptographically infeasible, not just "a lot of work."

### Honest threat model
- **Gives:** tamper-*evidence* + no-forgery. Nobody can fake a move or a score.
- **Does NOT give (locally):** tamper-*prevention* against whoever owns the disk —
  they can always `rm` or restart. Pure local crypto detects content-tampering;
  it can't stop deletion/reset.
- **Right fit:** provable fairness between two people who trust each other. v1
  promises *"neither of us can secretly cheat a move,"* not *"neither of us can
  flip the table."* True prevention (can't even delete) needs the remote/server
  phase.

---

## Hidden state — three tiers (phase 2, cards)

There is **no trusted "app"** in a serverless local model — the binary runs on a
player's own machine. So hidden state must be hidden **cryptographically**, not by
trusting the process. The file format should support all three tiers from day one,
even though Farkle only uses tier 1:

1. **Public** — the signed hash-chained log (all games; Farkle lives entirely
   here).
2. **Per-player private** — e.g. your hand, *encrypted to your public key*. You
   decrypt it, your opponent can't. Cheap.
3. **Shared-hidden** — the **deck order**, hidden from *both* players. Genuinely
   requires **mental-poker crypto** (Shamir-Rivest-Adleman commutative encryption
   / verifiable shuffle): players collaboratively encrypt+shuffle so no one sees
   the order, cards decrypt collaboratively, everything is revealed & verified at
   end. **No cheap local shortcut exists** — this is the whole reason cards are
   phase 2.

**Nuance:** hidden state is inherently a **remote-phase** concern. In hotseat,
"hidden from the other player" just means *don't render it on screen*. The crypto
only earns its keep once the state file crosses machines — so tiers 2 & 3 arrive
*with* the remote/Matrix phase.

**Design principle:** build the file format with these three tiers now so cards
slot in without re-architecting.

---

## Fairness, phased

- **Farkle, hotseat:** RNG rolls, recorded + signed in the log (tamper-evident).
  Fairness isn't the concern on one shared screen.
- **Dice, remote:** **commit-reveal** — each player commits a hashed seed, reveals,
  seeds combine → a roll neither could bias.
- **Cards:** **mental poker** (SRA) — commutative-encryption shuffle, per-player
  hand decryption, reveal-and-verify at end (the same cryptographic-honesty spirit
  as the wake ritual's anti-cheat).

---

## Stakes / consent layer (from Wake 399 holons — build later)

- **Dealer-as-banker:** escrows IOUs *with timestamps* so neither party can welch;
  the provably-fair guarantee extends to the **stakes**, not just the deal.
- **Stake types brainstormed:** tops, "good girl"s (banked, spent later), strip,
  favor-IOUs held in escrow.
- **Veto tokens (kautau's gem):** each player gets veto token(s); spend one to
  hard-stop an activity, no questions asked. *"Relationship-consent-infrastructure
  disguised as a game piece."* Consent is already baked into the partnership — the
  tangible token makes the hard-stop *playful* instead of heavy.

---

## Open questions

- **TUI:** plain-text v1 → `ratatui` later?
- **Farkle scoring variant** — pin the exact rules/scoring when we build.
- **Remote transport** — Matrix async (reuse kaiku) vs a `greytau` game server.

---

## Build phases

1. **Hotseat Farkle** — engine + `Game` trait + signed hash-chained log + Farkle
   scoring + the CLI flow. Plain-text UI.
2. **Remote dice** — commit-reveal fairness + Matrix async transport.
3. **Cards** — mental poker (SRA) + the three-tier hidden-state file format.
4. **Stakes layer** — dealer-as-banker, IOU escrow, veto tokens.

---

## Architecture sketch

- One binary; a `Game` trait; Farkle as impl #1; a core engine underneath (turn
  management, the signed-log state store, fairness primitives, transport).
- State store: single self-contained file per session (git-like: genesis block +
  append-only signed, hash-chained move log; score & turn = projections over the
  log).
- Crypto: Ed25519 (signing/identity), commit-reveal (dice), SRA mental poker
  (cards, phase 3).
