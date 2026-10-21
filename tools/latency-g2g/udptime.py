# Answers each UDP datagram with this machine's wall clock in microseconds.
import socket, time
s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM); s.bind(("0.0.0.0", 18766))
while True:
    data, addr = s.recvfrom(64)
    s.sendto(data + b" " + str(time.time_ns() // 1000).encode(), addr)
