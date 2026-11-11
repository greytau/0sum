#!/usr/bin/env python3
"""Independent cross-check of the Liar's Dice golden vectors (docs/PHASE2.md
§2.2, §4.1, §13, amendment A3), written from the spec text with the Python
standard library only. Dev script; CI doesn't run it.

    python3 tests/vectors/ld_vectors.py
"""
import hashlib
import hmac
import struct

SID = "TEST01"
ROUND = 1
PLAYERS = [("p1", "AAAAAAAAAAAAAAAAAAAAAAAAAA"), ("p2", "BBBBBBBBBBBBBBBBBBBBBBBBBB")]
DICE = 5


def sha256(*parts):
    return hashlib.sha256(b"".join(parts)).digest()


def hkdf_extract(salt, ikm):
    return hmac.new(salt, ikm, hashlib.sha256).digest()


def hkdf_expand_32(prk, info):
    # One block is enough: T(1) = HMAC(prk, info || 0x01).
    return hmac.new(prk, info + b"\x01", hashlib.sha256).digest()


def lp(s):
    b = s.encode()
    return struct.pack(">H", len(b)) + b


def u32be(n):
    return struct.pack(">I", n)


def bound(domain, seat, s):
    return domain + lp(SID) + u32be(ROUND) + lp(seat) + s


def dice(seat, s, others, count):
    d = sha256(bound(b"0sum/v2/liars-dice/dice\0", seat, s), *others)
    out, j = [], 0
    while len(out) < count:
        for b in sha256(d, u32be(j)):
            if len(out) == count:
                break
            if b < 252:
                out.append(b % 6 + 1)
        j += 1
    return out


def main():
    rows = []
    for seat, secret in PLAYERS:
        prk = hkdf_extract(("0sum/v2/" + SID).encode(), secret.encode())
        sign_seed = hkdf_expand_32(prk, b"0sum/v2/sign")
        s = hkdf_expand_32(prk, b"0sum/v2/liars-dice/secret" + u32be(ROUND))
        c = sha256(bound(b"0sum/v2/liars-dice/commit\0", seat, s))
        rows.append({"seat": seat, "sign_seed": sign_seed, "s": s, "C": c})
    cset = sha256(*(r["C"] for r in rows))
    for r in rows:
        r["n"] = sha256(bound(b"0sum/v2/liars-dice/entropy\0", r["seat"], r["s"]), cset)
    for r in rows:
        others = [o["n"] for o in rows if o["seat"] != r["seat"]]
        r["dice"] = dice(r["seat"], r["s"], others, DICE)

    print(f"SID = {SID}, round r = {ROUND}")
    for (seat, secret), r in zip(PLAYERS, rows):
        print(f"{seat} password {SID}-{secret}")
        for k in ("sign_seed", "s", "C", "n"):
            print(f"  {k:<9} {r[k].hex()}")
        print(f"  dice[{ROUND}]   {r['dice']}")
    print(f"CSET[{ROUND}]     {cset.hex()}")


if __name__ == "__main__":
    main()
