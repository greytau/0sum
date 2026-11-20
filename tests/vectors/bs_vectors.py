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
