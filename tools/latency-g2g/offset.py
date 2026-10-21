#!/usr/bin/env python3
"""Clock offset of the host machine minus this one, in ms, over UDP (udptime.py runs on the host):
the exchange with the shortest of 1000 round trips. Prints "<offset ms> <round trip ms>"; the
error is at most half the round trip.

  python3 offset.py <host ip>
"""
import socket, sys, time

host = sys.argv[1]
s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM); s.settimeout(1)
best = None
for i in range(1000):
    t0 = time.time_ns() / 1e6
    s.sendto(str(i).encode(), (host, 18766))
    try:
        data, _ = s.recvfrom(64)
    except socket.timeout:
        continue
    t1 = time.time_ns() / 1e6
    n, remote = data.split()
    if int(n) != i:
        continue
    if best is None or t1 - t0 < best[0]:
        best = (t1 - t0, int(remote) / 1000 - (t0 + t1) / 2)
    time.sleep(0.002)
print(f"{best[1]:.2f} {best[0]:.2f}")
