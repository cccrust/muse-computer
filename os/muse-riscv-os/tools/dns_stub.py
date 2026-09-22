#!/usr/bin/env python3
"""v1.7 host-side DNS stub for the guest nslookup test.

Listens on 127.0.0.1:5353 (the guest queries it as 10.0.2.2:5353 through
QEMU user-net). Answers every A query with 10.0.2.99 (TTL 60), preserving
the query ID and question section. Enough to prove the guest's query
building, compression-pointer parsing, and retry loop end to end.
Started/stopped by test.sh around run1.
"""
import socket
import struct

ADDR = ("127.0.0.1", 15353)  # NOT 5353: macOS mDNSResponder owns it
ANSWER_IP = (10, 0, 2, 99)


def reply(query: bytes) -> bytes | None:
    if len(query) < 12:
        return None
    qid = query[0:2]
    qdcount = struct.unpack(">H", query[4:6])[0]
    if qdcount != 1:
        return None
    # find question end: labels, then QTYPE/QCLASS
    o = 12
    while True:
        if o >= len(query):
            return None
        ln = query[o]
        if ln & 0xC0:
            return None
        if ln == 0:
            o += 1
            break
        if ln > 63 or o + 1 + ln > len(query):
            return None
        o += 1 + ln
    if o + 4 > len(query):
        return None
    question = query[12 : o + 4]
    head = qid + b"\x81\x80" + struct.pack(">HHHH", 1, 1, 0, 0)
    ans = (
        b"\xc0\x0c" + struct.pack(">HHIH", 1, 1, 60, 4) + bytes(ANSWER_IP)
    )
    return head + question + ans


def main() -> None:
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.bind(ADDR)
    print(f"dns_stub: listening on {ADDR[0]}:{ADDR[1]}", flush=True)
    while True:
        data, peer = s.recvfrom(512)
        r = reply(data)
        if r is not None:
            s.sendto(r, peer)


if __name__ == "__main__":
    main()
