# 0sum Phase 2: signed log, fair hidden dice, engine generalization, Liar's Dice

> Build spec. Commit this as `docs/PHASE2.md`. It replaces the "planned" crypto in
> `docs/DESIGN.md` with a concrete design. Written against `origin/main` @ `48441ca`.
> Breaking changes to the session format and CLI are pre-cleared. v1 sessions are not migrated.

---

## 0. The shape in one breath

Every session is one JSON file holding an append-only log. Each entry is hash-chained
to the previous one and signed (Ed25519) by its author. A player's password is a
high-entropy token minted by the binary. It contains the public session id plus a
130-bit secret, and HKDF turns it into a signing key and per-round game secrets.
Nothing secret is ever written to disk: no password index, no encrypted private
state. Private state (your Liar's Dice dice) is *recomputed* from your password plus
the public log on every invocation.

Hidden dice use a two-phase commit/entropy protocol with deterministic derivation:

1. Every live player commits to a secret `s` that is derived from their password.
2. Only once *all* commitments are in, every player publishes an entropy value `n`.
   This is also derived deterministically, from their password and the commitment set.
3. Your dice = `PRF(your s, everyone else's n)`, using rejection sampling, so there is no
   modulo bias. Your own `n` never feeds your own dice.
4. A challenge makes every live player reveal `s`. Anyone can then check `s` against the
   commitment, recompute everyone's dice, and check the count.

Protocol duties (commit, entropy, reveal) are **automatic side effects of any
invocation**. Nobody types "roll" or "reveal".

Farkle moves onto the same log. Its events get signed and chained, its gameplay doesn't
change, and it does not use hidden state.

---

## 1. Threat model

**Setting.** One machine. One shared disk. N ≥ 2 humans in separate terminals, all running `0sum`.
Any player can read **and edit** the session file and everything under `~/.0sum/`.
A player might also run a modified binary.

**Guarantees (all checked by `0sum verify`, and implicitly by every invocation):**

| # | Property | Mechanism |
|---|----------|-----------|
| G1 | Reading the disk reveals no password, no secret, no unrevealed die | Nothing secret is stored. The disk holds only pubkeys, commitments, entropy and signatures. |
| G2 | Nobody can forge, alter, reorder, or re-attribute another player's entry | SHA-256 hash chain plus a per-entry Ed25519 signature by the author |
| G3 | Nobody can choose or bias their own dice | Commit-before-entropy ordering (enforced in the log), own entropy excluded, rejection sampling |
| G4 | Nobody sees anyone else's dice before reveal, the roller included | Dice need the owner's secret `s`, which never leaves the owner's password |
| G5 | Every reveal and every challenge outcome is verifiable from the log alone | `s` is checked against the commitment, dice are re-derived, the count is recomputed |
| G6 | Signers can't sign illegal moves either | Every game event is replayed through the game's `fold`, which validates legality |

**Explicit non-guarantees** are in §12. Read them; they matter.

---

## 2. Identity: password → keys

### 2.1 Password format

```
<SID>-<SECRET>
 SID    = 6 Crockford base32 chars (the public session id, unchanged from v1)
 SECRET = 26 Crockford base32 chars = 130 bits from the OS RNG
example: TEST01-AAAAAAAAAAAAAAAAAAAAAAAAAA
```

- Alphabet: `0123456789ABCDEFGHJKMNPQRSTVWXYZ`. Generation: each char is `OsRng.next_u32() & 31`.
  32 is a power of two, so there is no bias.
- Parse: trim, ASCII-uppercase, split once on `-`, then validate both alphabets and lengths.
- The password is **only** ever minted by `0sum register`. There is no path that accepts a
  user-chosen password, and this is load-bearing (see 2.3).
- `Password` gets a hand-written `Debug` that prints `TEST01-<redacted>`.

### 2.2 Derivation (HKDF-SHA256)

```
prk  = HKDF-Extract(salt = "0sum/v2/" || SID,  ikm = SECRET as ASCII bytes)

sign_seed     = HKDF-Expand(prk, "0sum/v2/sign", 32)            -> ed25519 SigningKey::from_bytes
state_key     = HKDF-Expand(prk, "0sum/v2/state-key", 32)       -> RESERVED, unused in phase 2
game secrets  = HKDF-Expand(prk, "0sum/v2/<game-id>/<label>...", 32)
```

Liar's Dice labels (`r` = round number as big-endian u32, `‖` = concat):

```
s_i^r = HKDF-Expand(prk_i, "0sum/v2/liars-dice/secret"  ‖ u32be(r), 32)
n_i^r = HKDF-Expand(prk_i, "0sum/v2/liars-dice/entropy" ‖ u32be(r) ‖ CSET^r, 32)
CSET^r = SHA-256( C_k^r for every live seat k, in seat order, raw 32 bytes each )
```

The engine exposes this as a `Secrets` handle (§9.3). Games call
`secrets.derive(info)`, and `info` MUST start with `0sum/v2/<game-id>/`.

### 2.3 Why HKDF, not Argon2/scrypt

A slow KDF exists to make **guessing low-entropy human passwords** expensive. Here the
secret is 130 bits minted by the OS RNG. An offline attacker needs about 2^129 tries,
whatever the cost per try, so stretching buys nothing and costs latency on every invocation.

This is only true because users can't choose passwords. The public key on disk is a
perfect offline guessing oracle (guess, derive, compare). A memorable password would fall
to it in seconds, Argon2 or not. So "the binary mints every password" is a security
invariant, not a UX choice.

### 2.4 v2 uses no index (fixes #6)

The password carries its session id, so `play` needs no lookup table. v2 never reads or
writes `~/.0sum/index.json`: there's no file to dump, key, or chmod.

- `play` parses SID from the password and loads `sessions/<SID>.json`.
- It derives the pubkey, then finds the seat whose `register` entry carries that pubkey.
- If no seat matches, the error is `this password doesn't belong to any seat in session <SID>`.
- **Amended (ruling, 2026-11-11): the legacy index is never deleted.** Real v1 games may still
  live under `~/.0sum`. If a legacy `index.json` exists, print one line to stderr and touch
  nothing (no deletion, no chmod of v1 files):
  `warning: legacy v1 index.json at <path> holds plaintext passwords; v2 doesn't use it — delete it yourself when you're done with v1 games`.
  This supersedes "legacy `index.json` is removed" in §13: the test is that it survives a v2
  run and the warning prints.

### 2.5 The password never goes on the command line

A positional `play <password>` writes the password into shell history, and shell history is
on the shared disk, which breaks G1. So the password comes **only** from the environment
variable `ZEROSUM_PASSWORD`. (An env var name can't start with a digit.)

```console
$ read -rs ZEROSUM_PASSWORD && export ZEROSUM_PASSWORD   # once per terminal, paste, Enter
$ 0sum play bid 3 5
```

- An agent player whose shell doesn't persist can prefix per command:
  `ZEROSUM_PASSWORD=... 0sum play bid 3 5`. Non-interactive shells write no history.
- If the variable is unset: error `set ZEROSUM_PASSWORD (read -rs ZEROSUM_PASSWORD && export ZEROSUM_PASSWORD)`.
- If the first move token parses as a password (`^[0-9A-Z]{6}-[0-9A-Z]{26}$`
  case-insensitive), refuse with: `don't put your password on the command line — it's in
  your shell history now; use ZEROSUM_PASSWORD`. Do not act on it.

---

## 3. The log: envelope, chain, signatures

### 3.1 Entry

```rust
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub seq: u64,               // == index in log
    pub ts: u64,                // unix seconds, self-reported, informational only
    pub author: Option<String>, // seat id ("p1"); None only for genesis
    pub prev: Hex32,            // hash of previous entry; 32 zero bytes for genesis
    pub body: Body,
    pub hash: Hex32,            // stored for readability; verify recomputes and requires equality
    pub sig: Option<Hex64>,     // None only for genesis
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Body {
    Genesis(Genesis),
    Register(Register),
    Game(serde_json::Value),    // the game's Event, parsed strictly by the game on replay
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Genesis {
    pub session_id: String,
    pub game: String,              // registry id: "farkle" | "liars-dice"
    pub ruleset: serde_json::Value,// the game's Rules, parsed strictly on replay
    pub roster: Vec<Seat>,         // [{seat:"p1",name:"Alice"}, ...]
    pub nonce: Hex16,              // 16 random bytes: makes h0 unique per creation
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Register { pub seat: String, pub pubkey: Hex32 }
```

`Hex16/32/64` are newtypes over `[u8; N]` that (de)serialize as lowercase hex strings and
reject any other length or case on parse.

### 3.2 What the chain hashes

```
canonical(e) = serde_json::to_vec( &json!({
                   "seq": e.seq, "ts": e.ts, "author": e.author,
                   "prev": e.prev, "body": e.body }) )
h(e)         = SHA-256( "0sum/v2/entry\0" ‖ canonical(e) )
```

- Canonical bytes come **through `serde_json::Value`**. Without the `preserve_order` feature,
  `Value` maps are `BTreeMap`s, so keys come out sorted at every depth. Output is compact,
  integer-only, and has deterministic escaping. Field declaration order never matters, and a
  parsed-then-reserialized `Game(Value)` hashes the same as a freshly serialized typed event.
- **`serde_json`'s `preserve_order` feature must never be enabled.** Cargo feature unification
  could turn it on silently, so a golden-vector test (§13) guards it.
- An attacker who writes `1.0` instead of `1`, adds whitespace, or reorders keys changes
  nothing (whitespace and order are normalized away) or changes the hash (number type).
  Either way they can't produce a matching signature.

### 3.3 What is signed

```
sig(e) = Ed25519-Sign( sk_author, "0sum/v2/sig\0" ‖ h(e) )   // 12-byte prefix + 32-byte hash
```

`h(e)` covers `prev`, so each signature commits to the author's whole view of history up to
that entry, genesis (and so the ruleset and roster) included. Verification uses
`VerifyingKey::verify_strict`, which rejects malleable signatures and small-order keys. Genesis
is unsigned because nobody has keys yet. It gets locked by the first `register`, whose signed
`prev` is `h0`.

### 3.4 What `verify` checks (in order, stops at the first failure)

1. File parses with `deny_unknown_fields` everywhere. `version == 2`. The top-level
   `session_id` equals the filename stem and `log[0].body.genesis.session_id`.
2. For each entry `i`: `seq == i`; `prev == hash(log[i-1])` (zeros for `i = 0`);
   `hash == h(entry)` (recomputed).
3. **Genesis.** Only at `seq 0`. `author == None`, `sig == None`. `game` is in the registry.
   `ruleset` parses as `G::Rules`. Roster has ≥ 2 seats, seat ids are exactly `p1..pN` in
   order, and names are unique case-insensitively.
4. **Register.** `author == Some(seat)`. The seat exists and isn't registered yet. `pubkey` is a
   valid point not used by any other seat. `sig` verifies under **that** pubkey (proof of possession).
5. **Game.** Every seat is registered (the game has started). `author` is a registered seat.
   `sig` verifies under the author's registered key. The value parses strictly as
   `G::Event`. **`G::fold(state, author, event)` returns `Ok`**: game legality, including all
   Liar's Dice commit/entropy-ordering/reveal/dice checks.
6. Success: print `OK  <N> entries  head <seq>:<first 12 hex of hash>`.
   Failure: print `BROKEN at seq <i>: <reason>` and exit 1.

`ts` is never checked. It is self-reported, so it's display-only.

**Every** command that loads a session runs this full replay before acting (logs are
small) and refuses to append to a broken log. There is no repair command. A broken log is
evidence, so leave it alone.

---

## 4. Fair hidden dice (Liar's Dice)

### 4.1 Primitives

```
lp(x)  = u16be(len(x)) ‖ x

C_i^r  = SHA-256( "0sum/v2/liars-dice/commit\0" ‖ lp(SID) ‖ u32be(r) ‖ lp(seat_i) ‖ s_i^r )

D_i^r  = SHA-256( "0sum/v2/liars-dice/dice\0" ‖ lp(SID) ‖ u32be(r) ‖ lp(seat_i) ‖ s_i^r
                  ‖ n_k^r for every live seat k ≠ i, in seat order )

stream: block_j = SHA-256( D_i^r ‖ u32be(j) ),  j = 0, 1, 2, ...
dice:   walk the stream byte by byte; byte b < 252 → die (b mod 6) + 1; b ≥ 252 → skip.
        Take exactly dice_count(i) dice. Keep derivation order (render sorted).
```

- **Modulo bias.** 252 = 6 × 42, so accepted bytes map uniformly onto faces. The 4/256
  rejected bytes are skipped. The stream is unbounded, so derivation always finishes, at an
  expected `count × 256/252` bytes. Do **not** use `rand`'s `gen_range` here, because its
  algorithm changed between rand 0.8 and 0.9, and verification must be byte-reproducible forever.
- Domain prefixes, SID, round and seat in every hash stop anyone replaying a commitment
  across rounds, seats or sessions. They also stop anyone copying an opponent's commitment
  (you couldn't open it anyway).

### 4.2 The ordering rule (this is what defeats grinding)

Within round `r`, the log must satisfy, and `fold` enforces:

> An `entropy{r}` entry is valid **only if every live seat's `commit{r}` precedes it in the log.**

Consequences, each addressed explicitly:

- **Grinding your secret.** You fix `s_i` (via `C_i`) before any `n_k` exists anywhere, so at
  commit time your dice are a PRF of values nobody has produced yet. Trying many secrets gains
  nothing.
- **Last committer.** The last to commit sees only the other *commitments*, which are hiding
  (256-bit preimages). No entropy exists yet. Committing last is worth nothing.
- **Last entropy publisher.** Your `n_i` never enters your own dice (`k ≠ i`). You can't predict
  other players' dice without their `s`, so choosing your `n_i` adversarially is pointless.
  This holds for colluding groups too: colluders still don't know an outsider's `s`.
- **Rollback grinding, honest binary.** `s_i^r` is a function of (password, r). `n_i^r` is a
  function of (password, r, commitment set). Roll the file back to any earlier point and an
  honest binary republishes **byte-identical** commits and entropy, so the same dice come out.
  Editing the file can never re-roll anything. This is why both values are derived
  deterministically instead of drawn from an RNG.
- **Rollback grinding, modified binary.** A cheater can roll back to before their own commit and
  commit a fresh secret. Because `n_k` is bound to the commitment set, the honest opponent's
  binary then produces a fresh, unpredictable `n_k`. The cheater gets a random *re-draw* and
  still can't *choose*. They also need the opponent to re-run without noticing that the log
  lost entries (§12).
- **Abort / last revealer.** There is **no re-roll mechanism** in the rules. A committed round
  must complete. The only leverage a player gets from refusing to publish entropy or reveal
  is stalling the game forever, which is "flipping the table" (§12). There's no better outcome
  to fish for. And a reveal fires automatically on *any* invocation by that player (§7), so
  withholding one takes deliberate effort, which is visible.

### 4.3 Reveal & verification

Once a call (`liar`/`spot`) is made, every live seat appends `reveal{r, s}`. `fold` checks
`C(s) == commits[author]`, then derives that seat's dice from `s` and the logged entropy of
the other live seats. When the last live seat has revealed, `fold` resolves the round (§10).
All of this is pure computation over the log, so any verifier reproduces it exactly (G5).

Nobody can verify that `n_k` was *derived from k's password*. It's a keyed PRF, and it doesn't
need verifying, because the security argument above never relies on it. (A VRF would make it
checkable, but buys nothing here.)

---

## 5. Viewing your own private state

There is no private file. With `ZEROSUM_PASSWORD` set, `0sum play` with no moves:
1. runs any pending duties (§7),
2. derives `s_me^r`, reads the other live seats' `n_k^r` from the log, and computes your dice,
3. renders the public state plus `your dice: …`.

Your dice are visible as soon as every *other* live seat has published entropy. Your own
entropy isn't needed for your dice. Before that, the render says `waiting on <names> to roll`.
Reading the disk without your password yields commitments only (G1, G4).

`state_key` is derived and reserved. Phase 2 encrypts nothing, because a deterministic
derivation of private state removes the need to store it. If a later game truly needs stored
private state, use `chacha20poly1305` (XChaCha20-Poly1305) keyed by `state_key`, and not before.

---

## 6. Storage

```
$XDG_CONFIG_HOME/0sum or ~/.0sum/        mode 0700
  sessions/                              mode 0700
    <SID>.json                           mode 0600   the session (the only state)
    <SID>.lock                           mode 0600   advisory lock target, empty
```

- **Modes** (`#[cfg(unix)]`). Create dirs with `DirBuilder::mode(0o700)`. On *every* run, also
  `set_permissions` the existing dirs to 0700 and the session file to 0600.
  `OpenOptions::mode` only applies at creation (the #6 trap). These modes protect against
  *other unix users*. A player who shares your uid can read everything anyway, which is
  exactly why §2/§4 store nothing secret.
- **Atomic write.** Write `<SID>.json.tmp` (create, truncate, 0600), then `rename` over `<SID>.json`.
- **Lock.** Two terminals act at the same time, and the commit phase is simultaneous by nature.
  Every load → verify → append → save cycle holds an exclusive `std::fs::File::lock()` on
  `<SID>.lock`. This is std since Rust 1.89, so set `rust-version = "1.89"`. Lock the
  separate `.lock` file, **not** the JSON: the rename swaps the JSON's inode, which would drop
  the lock. The lock is advisory and only prevents honest lost updates. It is not a security
  boundary.
- **`new`** generates SIDs until `OpenOptions::create_new` succeeds on `<SID>.json`.
- **v1 files** (`"version": 1`) error out with: `session <SID> is a v1 (unsigned) session and can't be played by this version — start a new one`.

---

## 7. CLI and the round flow

### 7.1 Commands

```
0sum new <game> [--ruleset NAME]   prompts player count + names; prints SID + register commands
0sum register <SID> <name>         mints a password, appends register, runs duties if the game just started
0sum play [MOVE...]                password from ZEROSUM_PASSWORD; duties, then moves; no moves = status
0sum verify <SID>                  public, no password; full replay; prints narrative + head or BROKEN
```

Game ids: `farkle`, plus `liars-dice` (aliases `liarsdice`, `ld`). Liar's Dice moves:
`bid <qty> <face>` (alias `b`), `liar` (aliases `l`, `call`), `spot` (alias `exact`; only if
the ruleset enables it). Farkle moves are unchanged: `roll`, `keep <dice…>`, `bank`, chaining.

### 7.2 Duties: the automation

Every invocation that holds a password (`register`, `play`) runs **duties before the first move,
between moves, and after the last move**. A duty is the next protocol entry this seat owes, as
reported by `G::duty`. Duties repeat until none is owed. For Liar's Dice the order is:

1. `Revealing`, I'm live, not revealed yet → `reveal{r, s_me^r}`
2. `Revealing`, I've revealed, no next-round commit yet → `commit{r+1, C_me^{r+1}}` (pipelined; ignored if I end up eliminated)
3. `Rolling`, I'm live, not committed → `commit{r, C_me^r}`
4. `Rolling`, I'm live, no entropy yet, **all** live seats committed → `entropy{r, n_me^r}`

So nobody ever types roll or reveal. The minimum human action per round is "look, then act".

### 7.3 A full 2-player round, exactly

Session `TEST01`, ruleset `classic`. Alice is `p1` with `TEST01-AAAAAAAAAAAAAAAAAAAAAAAAAA`,
Bob is `p2` with `TEST01-BBBBBBBBBBBBBBBBBBBBBBBBBB`. These are the §13 test-vector passwords,
so the dice below are the real derived values.

| # | Who runs | Command | Entries appended (seq · author · body) | Prints |
|---|---|---|---|---|
| 1 | anyone | `0sum new ld` (Alice, Bob) | `0 · — · genesis` | SID, register lines |
| 2 | Alice | `0sum register TEST01 Alice` | `1 · p1 · register` | password, `read -rs` hint, waiting on Bob |
| 3 | Bob | `0sum register TEST01 Bob` | `2 · p2 · register` → game started → duty: `3 · p2 · commit{1}` | password; waiting on Alice to roll |
| 4 | Alice | `0sum play` | duties: `4 · p1 · commit{1}`, `5 · p1 · entropy{1}` (all committed) | rolled; waiting on Bob's roll |
| 5 | Bob | `0sum play` | duty: `6 · p2 · entropy{1}` → phase Bidding, Alice starts | **your dice: 2 3 4 5 5**; Alice bids first |
| 6 | Alice | `0sum play` | — | **your dice: 1 2 5 6 6**; your bid |
| 7 | Alice | `0sum play bid 2 6` | `7 · p1 · bid{2,6}` | |
| 8 | Bob | `0sum play bid 3 5` | `8 · p2 · bid{3,5}` | |
| 9 | Alice | `0sum play bid 3 6` | `9 · p1 · bid{3,6}` | |
| 10 | Bob | `0sum play liar` | `10 · p2 · call{liar}`, then duties `11 · p2 · reveal{1}`, `12 · p2 · commit{2}` | waiting on Alice's reveal |
| 11 | Alice | `0sum play` | duty `13 · p1 · reveal{1}` → **resolves**: sixes = 6,6 + wild 1 = 3 ≥ 3, so caller Bob loses a die (4 left); round 2, starter Bob. Duties: `14 · p1 · commit{2}`, `15 · p1 · entropy{2}` | round result with all dice; waiting on Bob's roll |
| 12 | Bob | `0sum play` | duty `16 · p2 · entropy{2}` → Bidding, Bob starts | round 1 result; **your dice** (4); you bid first |

Steps 6 and 11 are "look" invocations. Each player always needs one invocation after the
last entropy they depend on before they can see dice. That's the information-theoretic floor
for asynchronous play without a co-signer running in the background.

---

## 8. On-disk schema (v2)

```json
{
  "version": 2,
  "session_id": "TEST01",
  "log": [
    { "seq": 0, "ts": 0, "author": null,
      "prev": "0000000000000000000000000000000000000000000000000000000000000000",
      "body": { "genesis": {
          "session_id": "TEST01", "game": "liars-dice",
          "ruleset": { "name": "classic", "dice_per_player": 5, "wild_ones": true, "spot_on": false },
          "roster": [ { "seat": "p1", "name": "Alice" }, { "seat": "p2", "name": "Bob" } ],
          "nonce": "00000000000000000000000000000000" } },
      "hash": "4c3c06def777ecf293af7b1fa324f46422ef4d137f8a0bcab71dd943d7040dee",
      "sig": null },
    { "seq": 1, "ts": 0, "author": "p1",
      "prev": "4c3c06def777ecf293af7b1fa324f46422ef4d137f8a0bcab71dd943d7040dee",
      "body": { "register": { "seat": "p1",
          "pubkey": "c14f1c546e873389fc5442bc5379bae199b5a514b6cdea5e4a5914ca01d28178" } },
      "hash": "2c0830ef6065e388e48a5db116d0acd630386b8d6c936612070ddc4eb128cd2c",
      "sig": "7e9f9f475e42da3e8a3724967ce7c540d8025bae70c4eada55b7a6196e44449d882b39b8fdfbe44119220aa01105fec20a4e8e0a229684dbbb44116fd28bf201" },
    { "seq": 3, "author": "p2", "body": { "game": { "commit":  { "round": 1, "c": "9dddb0…72dd" } } }, "…": "…" },
    { "seq": 5, "author": "p1", "body": { "game": { "entropy": { "round": 1, "n": "040563…b4a6" } } }, "…": "…" },
    { "seq": 7, "author": "p1", "body": { "game": { "bid":     { "qty": 2, "face": 6 } } }, "…": "…" },
    { "seq": 10,"author": "p2", "body": { "game": { "call":    { "kind": "liar" } } }, "…": "…" },
    { "seq": 11,"author": "p2", "body": { "game": { "reveal":  { "round": 1, "s": "24480e…ad9e" } } }, "…": "…" }
  ]
}
```

(Abbreviated entries show only the fields that differ. Every real entry has all seven fields.)

Liar's Dice events: `commit{round:u32, c:Hex32}`, `entropy{round:u32, n:Hex32}`,
`bid{qty:u8, face:u8}`, `call{kind:"liar"|"spot"}`, `reveal{round:u32, s:Hex32}`. Reveals
carry no dice: the dice are derived, so they can't disagree with themselves.

Farkle events: `rolled{dice:[u8]}`, `kept{dice:[u8]}`, `banked{}`. **Dropped** from v1:
`kept.points`, `banked.points`, and the whole `busted` event. All of those are projections:
`fold` computes points, and a scoreless `rolled` *is* the bust. Under "store moves, derive
state" a derived field in a signed entry is just one more thing that can disagree.
`SessionCreated`/`PlayerRegistered` become engine-level `genesis`/`register`.

---

## 9. Engine generalization

### 9.1 Layout

```
src/main.rs               CLI + registry (dispatch)
src/engine/mod.rs         Game trait, Ctx, Table<G>, replay, append, apply_chain
src/engine/log.rs         Entry/Body/Genesis/Register, Hex newtypes, canonical, h(), sign/verify
src/engine/keys.rs        Password (gen/parse), Secrets (HKDF), signing key
src/storage.rs            paths, modes, lock, atomic write, legacy cleanup
src/farkle/mod.rs         impl Game for Farkle          (was game.rs + the Farkle half of session.rs)
src/farkle/ruleset.rs     unchanged content             (git mv)
src/farkle/scoring.rs     unchanged content             (git mv)
src/liars_dice/mod.rs     impl Game for LiarsDice
src/liars_dice/ruleset.rs §10
src/liars_dice/dice.rs    §4.1 commitment + derivation (pure fns, vector-tested)
```

### 9.2 The trait

Games are zero-sized types with associated functions. No `&self`, no trait objects inside the engine.

```rust
pub trait Game: 'static {
    const ID: &'static str;
    const DEFAULT_RULESET: &'static str;
    type Rules: Serialize + DeserializeOwned + Clone;
    type State: Clone;
    type Move: Clone + Debug + PartialEq;
    type Event: Serialize + DeserializeOwned + Clone + Debug;   // #[serde(deny_unknown_fields)]

    fn ruleset(name: &str) -> Result<Self::Rules, String>;
    fn describe(rules: &Self::Rules) -> String;                  // one line for new/register output
    fn initial(rules: Self::Rules, info: &SessionInfo) -> Self::State; // info = { session_id, seats }
    fn parse_moves(tokens: &[String]) -> Result<Vec<Self::Move>, String>;

    /// THE rules engine: validate `ev` by `author` against `state`, then apply it.
    /// Used identically by live play and by verify-replay. Err = illegal.
    fn fold(state: &mut Self::State, author: usize, ev: &Self::Event) -> Result<(), String>;

    /// Turn a player's intent into events (may consume ctx.rng / ctx.secrets).
    fn plan(state: &Self::State, me: usize, mv: &Self::Move, ctx: &mut Ctx)
        -> Result<Vec<Self::Event>, String>;

    /// Next protocol entry this seat owes right now. Farkle: always None.
    fn duty(_state: &Self::State, _me: usize, _ctx: &Ctx) -> Option<Self::Event> { None }

    fn to_move(state: &Self::State) -> Option<usize>;   // None = nobody (protocol wait / game over)
    fn winner(state: &Self::State) -> Option<usize>;

    fn narrate(before: &Self::State, author: usize, ev: &Self::Event, after: &Self::State,
               names: &[String]) -> Vec<String>;
    fn render(state: &Self::State, names: &[String], me: Option<usize>, ctx: Option<&Ctx>) -> String;
}

pub struct Ctx { pub secrets: Secrets, pub rng: StdRng }
```

`fold` merges today's `project()` (fold) with validation that today lives only in `apply()`.
Now that logs are adversarial, replay has to validate. A signature proves *who* wrote an
entry, not that the entry was legal.

### 9.3 Tier-2 hidden state plugs in through `Ctx`

`Ctx.secrets` is the password-derived `Secrets` handle (`derive(info) -> [u8;32]`,
`signing_key()`). Liar's Dice uses it in `duty` (commit/entropy/reveal values) and `render`
(your dice). Farkle never touches it: Farkle uses `ctx.rng` (still
`StdRng::from_entropy()` in production, seeded in tests). Verify-replay needs no `Ctx`,
because `fold` is pure over public data. That asymmetry is the tier boundary: **secrets only
ever flow into producing events and rendering, never into validating them.** Tier 3
(shared-hidden deck) is still phase 3 and not designed here.

### 9.4 Engine core

```rust
pub struct Table<G: Game> {
    pub session_id: String, pub names: Vec<String>, pub seats: Vec<String>,
    pub keys: Vec<Option<VerifyingKey>>, pub state: G::State, pub head: [u8; 32],
}
impl<G: Game> Table<G> { pub fn started(&self) -> bool { self.keys.iter().all(Option::is_some) } }

pub fn replay<G: Game>(s: &Session) -> Result<Table<G>, Broken>;         // §3.4; Broken { seq, reason }
pub fn append<G: Game>(s: &mut Session, t: &mut Table<G>, author: usize,
                       ev: G::Event, sk: &SigningKey) -> Result<(), String>; // fold FIRST, then push signed entry

pub struct Step<G: Game> { pub author: usize, pub event: G::Event, pub before: G::State, pub after: G::State }
pub struct Chain<G: Game> { pub steps: Vec<Step<G>>, pub failure: Option<String> }

/// Issue #8's seam, generalized. No I/O, no printing, no entropy source of its own.
pub fn apply_chain<G: Game>(s: &mut Session, t: &mut Table<G>, me: usize,
                            moves: &[G::Move], ctx: &mut Ctx, sk: &SigningKey) -> Chain<G>;
```

`apply_chain` semantics. These preserve every Farkle boundary property from #7.

```
run_duties()
for mv in moves:
    if winner(state).is_some()      -> failure "the game is over"; break
    match to_move(state):
        Some(me)                    -> ok
        Some(other)                 -> failure "it's <other>'s turn, not yours"; break
        None                        -> failure "waiting on <render's wait reason>"; break
    events = plan(...)?             (Err -> failure; break)
    for ev in events: append(...)?  (Err -> failure; break both)
    run_duties()
```

The legal prefix persists. A turn-ending move halts the chain via the `to_move` check, and a
win halts via the `winner` check. `cmd_play` becomes: lock → load → replay → resolve seat
from pubkey → `apply_chain` → save if anything was appended → print `narrate` for each step,
then `render`. **#8 falls out naturally.** Its tests (seeded `StdRng`, fixed-password
`Secrets`, an in-memory `Session`) need no filesystem.

### 9.5 Registry

```rust
trait Driver { fn new(&self, ruleset: Option<&str>) -> Result<(), String>;
               fn register(&self, s: Session, name: &str) -> Result<(), String>;
               fn play(&self, s: Session, pw: &Password, tokens: &[String]) -> Result<(), String>;
               fn verify(&self, s: &Session) -> Result<(), String>; }
struct Engine<G>(PhantomData<G>);
impl<G: Game> Driver for Engine<G> { /* generic bodies */ }

fn driver(game: &str) -> Result<&'static dyn Driver, String> {
    match game.to_ascii_lowercase().as_str() {
        "farkle" => Ok(&Engine::<Farkle>(PhantomData)),
        "liars-dice" | "liarsdice" | "ld" => Ok(&Engine::<LiarsDice>(PhantomData)),
        other => Err(format!("unknown game `{other}` (available: farkle, liars-dice)")),
    }
}
```

`new` dispatches on its argument. `register`, `play` and `verify` load the session, read
`genesis.game`, and dispatch on that. One `match` is the whole registry. It doesn't need to be
a plugin system.

### 9.6 Farkle: yes to chain + signing, and here's what that does and doesn't buy

Farkle adopts the whole envelope. It's shared machinery, and it gets G2 (no forging or editing
of the other player's moves) plus G6 (no illegal keeps or banks) for free. Gameplay and output
are unchanged apart from the password hint lines, which become `0sum play roll`, and `0sum play`
with no move, which now shows status instead of erroring.

**It does not make Farkle's dice fair.** The roller writes `rolled{dice}`, so a roller who edits
the file can sign any dice they like, and nothing can tell. Fair *public* dice for a push-your-luck
game need fresh opponent entropy **per roll**, chained rolls included (#8's note). Pre-committed
schemes let the roller foresee their own future rolls, which in Farkle is as bad as choosing
them. Doing it properly needs the opponent online as a co-signer (a `0sum watch` loop that
auto-answers roll requests). Out of scope for this phase. See §14.

---

## 10. Liar's Dice ruleset

`src/liars_dice/ruleset.rs`, in the same style as Farkle's:

```rust
//! Declarative Liar's Dice rules. Pure data; mechanics read every variant from here.

/// Stored in genesis so every session is self-describing and reproducible.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleSet {
    pub name: String,
    /// Dice each player starts with.
    pub dice_per_player: u8,
    /// 1s count toward every bid face, and 1s cannot themselves be bid.
    /// When false, 1s are an ordinary face (the lowest) and can be bid.
    pub wild_ones: bool,
    /// Enables the `spot` call: claim the last bid is EXACTLY right.
    pub spot_on: bool,
}

impl RuleSet {
    pub fn by_name(name: &str) -> Result<RuleSet, String> {
        match name.to_ascii_lowercase().as_str() {
            "classic" => Ok(RuleSet::classic()),
            "natural" => Ok(RuleSet::natural()),
            other => Err(format!("unknown ruleset `{other}` (available: classic, natural)")),
        }
    }
    /// The default. Wild 1s, no spot-on.
    pub fn classic() -> RuleSet { RuleSet { name: "classic".into(), dice_per_player: 5, wild_ones: true,  spot_on: false } }
    /// Proves the abstraction: no wild 1s (1s biddable as the lowest face), spot-on enabled.
    pub fn natural() -> RuleSet { RuleSet { name: "natural".into(), dice_per_player: 5, wild_ones: false, spot_on: true } }
}
```

### 10.1 Fixed rules (not variants)

- **Seats** act in roster order (`p1, p2, …`), skipping eliminated seats, wrapping around.
- **Round 1 starter:** `p1`. **Later rounds:** the loser of the previous round starts. If the loser
  was eliminated, the next live seat after them starts.
- **Faces that can be bid:** `2..=6` if `wild_ones`, else `1..=6`.
- **Legal bid** `(q, f)`: `f` can be bid, and `1 ≤ q ≤ total dice held by live seats`.
- **Ordering (a strict total order):** `(q, f)` beats `(q₀, f₀)` iff `q > q₀`, or `q == q₀ && f > f₀`.
  Any legal bid can open a round. The cap on `q` makes the order finite: after `(total, 6)`
  the only move left is a call.
- **Calls.** Only the seat to move may call, and only once the round has at least one bid. The
  bidder is the author of the last bid.
- **Count** for face `f` = number of live dice showing `f`, plus, if `wild_ones && f != 1`,
  the number showing `1`.
- **Resolution: exactly one die is lost per round.**
  - `liar`: `count ≥ q` → caller loses a die; otherwise bidder loses a die.
  - `spot`: `count == q` → bidder loses a die; otherwise caller loses a die.
- **Elimination** at 0 dice. **Win:** the last seat holding dice.

### 10.2 Why these defaults

- **5 dice, wild 1s:** the common core of every mainstream variant. Wild 1s make each die a
  1/3 match for any face, which keeps bidding lively.
- **1s not biddable when wild:** with that rule the bid order is a plain lexicographic total
  order. The alternative is Perudo's aces rule (halve to switch into 1s, double-plus-one to
  switch out), which is the most often misremembered rule in the family and the hardest to
  verify at a glance. It can be a later named ruleset (`perudo`) with a new field. Don't add the
  field now.
- **Spot-on off by default:** traditions disagree on its payoff (bidder loses / everyone else
  loses / caller regains a die). Where it's on (`natural`), "bidder loses one" keeps the
  one-die-per-round invariant, so game length is bounded and "loser starts" stays well defined.
- **Loser starts:** standard practice. It gives the trailing player initiative, and it's always defined.

### 10.3 State and fold (normative)

```rust
pub enum Phase { Rolling, Bidding, Revealing, Over }
pub enum Call { Liar, Spot }                         // serde: "liar" | "spot"
pub struct State {
    rules: RuleSet, sid: String, seats: Vec<String>,
    dice: Vec<u8>,                       // held per seat; 0 = eliminated
    round: u32,                          // 1-based
    phase: Phase, starter: usize, turn: usize,
    commits: Vec<Option<[u8; 32]>>,      // round `round`
    next_commits: Vec<Option<[u8; 32]>>, // round+1, gathered during Revealing
    entropy: Vec<Option<[u8; 32]>>,
    bids: Vec<(usize, u8, u8)>,          // (author, qty, face)
    call: Option<(usize, Call)>,
    revealed: Vec<Option<Vec<u8>>>,
    last: Option<RoundResult>,           // for render: bids, call, all dice, count, loser
    winner: Option<usize>,
}
```

| Event by `a` | Valid iff | Effect |
|---|---|---|
| `commit{k,c}` | (`Rolling`, `k==round`, `a` live, `commits[a]` empty) **or** (`Revealing`, `k==round+1`, `revealed[a]` set, `next_commits[a]` empty) | store in `commits` / `next_commits` |
| `entropy{k,n}` | `Rolling`, `k==round`, `a` live, `entropy[a]` empty, **every live seat has `commits` set** | store; if every live seat has entropy → `Bidding`, `turn = starter` |
| `bid{q,f}` | `Bidding`, `a == turn`, legal and beats the last bid | push; `turn = next_live(a)` |
| `call{kind}` | `Bidding`, `bids` non-empty, `a == turn`, `kind==Spot ⇒ rules.spot_on` | `call = (a, kind)`; `Revealing` |
| `reveal{k,s}` | `Revealing`, `k==round`, `a` live, `revealed[a]` empty, `C(sid,k,seat_a,s) == commits[a]` | `revealed[a] = derive(...)`; if every live seat revealed → resolve |

**Resolve:**
1. Compute the count, then the loser (§10.1). `dice[loser] -= 1`. Record `last`.
2. If one seat is left live: set `winner` and `Over`.
3. Otherwise:
   - `round += 1`.
   - `starter = loser` if they're still live, else `next_live(loser)`.
   - `commits = next_commits` with eliminated seats cleared.
   - Clear `next_commits`, `entropy`, `bids`, `call` and `revealed`.
   - Set `Rolling`.

`to_move` = `Some(turn)` in `Bidding`, else `None`. Duties follow §7.2. `plan` maps
`bid q f` to `[bid{q,f}]`, `liar` to `[call{liar}]` and `spot` to `[call{spot}]`, and lets
`fold` produce the error text. `parse_moves` accepts exactly one move: Liar's Dice has no
legal chains.

---

## 11. Crates

All pure Rust. No native libs, no network services. Versions are the ones in the local cargo cache.

| Crate | Major | For |
|---|---|---|
| `ed25519-dalek` | 2 (2.2) | Signing keys from a 32-byte seed, `sign`, `verify_strict`. Default features. No `rand_core` needed, since keys come from HKDF. |
| `sha2` | 0.10 | SHA-256: chain hash, commitments, CSET, dice stream. Already a transitive dep of dalek, so it's one hash family. |
| `hkdf` | 0.12 | HKDF-SHA256: password → sign seed, state key, game secrets |
| `hex` | 0.4 | Hex encoding for the `Hex16/32/64` newtypes |
| `rand` | 0.8 (existing) | `OsRng` for password, SID and genesis nonce; `StdRng` for Farkle rolls. Not used for hidden dice. |
| `serde`, `serde_json`, `clap` | existing | `serde_json` **without** `preserve_order` |

std `File::lock` replaces any locking crate (`rust-version = "1.89"`). SHA-256 instead of #6's
suggested BLAKE3 keeps everything in one hash family that's already in the tree. Not added:
`blake3`, `argon2` (§2.3), `chacha20poly1305` (§5), `rpassword` (the env var covers it).

---

## 12. What this does NOT protect against

Said plainly, so nobody overclaims:

1. **Deleting or resetting** the session file or `~/.0sum/`. You get detection only by noticing it's gone.
2. **Truncation / rollback.** Every prefix of a valid log is itself a valid log. A player can cut
   the file back to an earlier point, and nothing in the file can prove it. **Concrete exploit:**
   after the reveals, Alice knows Bob's dice. She truncates back to before her own last bid and
   bids again with full knowledge. Bob finds out only if he notices his call or the round is
   gone. Mitigation is out-of-band: every invocation prints `head <seq>:<12 hex>`, and `verify`
   prints the whole history. Once logs cross machines (the remote phase), a rollback-and-replay
   becomes **provable**, because the cheater will have signed two different children of the
   same `prev` (equivocation).
3. **Stalling.** Refusing to commit, publish entropy, reveal, or move. There's no trusted clock,
   so there are no timeouts. The log shows exactly who owes what (`waiting on Alice's reveal`),
   so the stall is attributable, never ambiguous.
4. **Same-uid OS access.** If players share a unix account, the opponent can read your shell
   history, your environment (`/proc/<pid>/environ`), your terminal scrollback, and an agent's
   transcripts. Anyone who reads your password **is** you. The design keeps the password off
   disk and off the command line, and that is all a program can do.
5. **Modified binary + undetected rollback** gives a random *re-draw* of your own Liar's Dice dice
   (§4.2). Not a choice, and it needs the opponent to re-run without noticing.
6. **Farkle roll honesty** (§9.6). The roller can sign dice of their choosing.
7. **Genesis before the first registration** is unsigned. `register` prints the game, ruleset and
   roster it's about to sign over, and registering means consenting to exactly that. After the
   first `register`, genesis is locked.
8. **Collusion and table talk** in N > 2 games. Inherent to the game.
9. **Timestamps** are self-reported and display-only.

---

## 13. Tests and golden vectors

These vectors come from `ed25519-dalek 2.2`, `sha2 0.10`, `hkdf 0.12`, `serde_json 1`,
following §2/§3/§4 exactly. Put them in tests as literals.

```
SID = TEST01, round r = 1
p1 password TEST01-AAAAAAAAAAAAAAAAAAAAAAAAAA
  sign_seed 06e3cefdb9a2f24c18d37044730b9158bd14fcfe924533028d534a1779f28320
  pubkey    c14f1c546e873389fc5442bc5379bae199b5a514b6cdea5e4a5914ca01d28178
  s[1]      612f661662e64504ef80d2ce7dcb3171d12fb3a73733e272eab3a79a18a6efb3
  C[1]      d7f50e01f67658bface606486dff390228e09589e4ae46f60295ba7c6a196128
  n[1]      040563f999e0bfad9b4426d3755008fece7ebe75829da2474745ce1bc0fab4a6
  dice[1]   [5, 6, 2, 1, 6]            (5 dice; others = [n_p2])
p2 password TEST01-BBBBBBBBBBBBBBBBBBBBBBBBBB
  sign_seed edab3f381eaa898c8d2b40af741b7313bb5712b428dc05a5f1a32b2e047284e0
  pubkey    53186d38e72218ba73d8db682147897eb029c4d298bfdf56e4a5ef3c8206f2e7
  s[1]      24480e09e67297d9b0c2cf278727d3df6c636c2d122d01a7ed6f47421bd8ad9e
  C[1]      9dddb09c7c16cc8a2cba8b6db9ec56469d97580477b6da97f264d93f4b1c72dd
  n[1]      d009b4d74f630cd494d7b54cd1b6abae43368b26741af5821f4eac2333f14f32
  dice[1]   [3, 5, 2, 4, 5]            (others = [n_p1])
CSET[1]     2998cfcb2d78a9012bdcc6a131d8a4bf04e11939cfc0f7b8d89806e4582614f0

genesis (§8, ts 0, nonce zeros) canonical bytes:
{"author":null,"body":{"genesis":{"game":"liars-dice","nonce":"00000000000000000000000000000000","roster":[{"name":"Alice","seat":"p1"},{"name":"Bob","seat":"p2"}],"ruleset":{"dice_per_player":5,"name":"classic","spot_on":false,"wild_ones":true},"session_id":"TEST01"}},"prev":"0000000000000000000000000000000000000000000000000000000000000000","seq":0,"ts":0}
h0   4c3c06def777ecf293af7b1fa324f46422ef4d137f8a0bcab71dd943d7040dee
register p1 (seq 1, ts 0, prev h0): h1 2c0830ef6065e388e48a5db116d0acd630386b8d6c936612070ddc4eb128cd2c
sig1 7e9f9f475e42da3e8a3724967ce7c540d8025bae70c4eada55b7a6196e44449d882b39b8fdfbe44119220aa01105fec20a4e8e0a229684dbbb44116fd28bf201
```

The genesis canonical bytes are also the `preserve_order` tripwire. If keys ever come out
unsorted, that test fails.

Required tests (beyond the vectors):
- **Tamper suite**, against a valid LD log with a full round:
  - editing a bid's qty → hash mismatch
  - editing it and recomputing `hash` → signature failure
  - changing an entry's `author` → signature failure
  - deleting a middle entry → `prev` mismatch
  - adding an unknown field → parse failure
  - editing genesis `ruleset` after a register → `prev` mismatch on seq 1
- **Ordering:** an `entropy{r}` placed before the last live `commit{r}` (with valid signatures) → `fold` rejects.
- **Reveal:** a wrong `s` → rejected. Round resolution with the vector dice → `3×6` is true,
  caller loses, `dice == [5, 4]`, round 2 starter `p2`.
- **Determinism:** run duties twice from the same truncated prefix and get byte-identical
  commit/entropy entries (rollback can't re-roll).
- **Rules:** a bid-ordering table (including the `wild_ones` face range and the qty cap);
  `spot` rejected under `classic`; wild counting; elimination plus the starter-skip rule;
  a seeded full 3-player game reaches exactly one winner.
- **`apply_chain`** (closes #8):
  - Farkle: legal-prefix-persists, halt at turn boundary, bust halts, win halts and reports.
  - LD: duties run when called with no moves; `bid` during `Rolling` → "waiting on …".
- **All existing Farkle tests** pass after the move. `plays_a_full_game_to_a_win` runs through
  `apply_chain` + `replay`, and the final `replay` equals the live state.
- **Storage:** after any write, modes are 0700 dirs and 0600 file; legacy `index.json` is removed;
  a v1 file is refused with the message.

---

## 14. Build order and what's out of scope

**PR 1, the engine (no new game):** `engine/{log,keys}`, storage, the trait, Farkle moved
onto it, `verify`, the env-var password, index deletion. Closes #6 and #8. All Farkle tests green.

**PR 2, Liar's Dice:** `liars_dice/{ruleset,dice,mod}`, the vectors, the tamper and rules suites.

**Out of scope, deliberately:**
- `0sum watch`, a background co-signer loop that auto-runs duties. It's the big UX win for
  two-terminal play and the prerequisite for fair Farkle rolls (§9.6).
- The `perudo` ruleset.
- Encrypted private state (§5).
- Remote transport.
- Tier-3 mental poker.

Update the README CLI examples to `export ZEROSUM_PASSWORD` + `0sum play …`, and add a pointer
from DESIGN.md to this file. DESIGN.md otherwise stays as the phase-1 record.
