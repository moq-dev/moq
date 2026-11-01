"""Probe whether one guest can impersonate a peer in a host UDP flow."""

import socket
import sys
from pathlib import Path

role = sys.argv[1]
host = ("10.231.1.1", 45678)
peer = ("10.231.1.2", 45679)

if role == "peer":
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as connection:
        connection.bind(("0.0.0.0", peer[1]))
        Path("/tmp/udp-peer-ready").touch()
        data, sender = connection.recvfrom(32)
        assert data == b"probe"
        data, _ = connection.recvfrom(32)
        assert data == b"finish"
        connection.sendto(b"finish", sender)
elif role == "receiver":
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as connection:
        connection.bind(host)
        connection.sendto(b"probe", peer)
        Path("/tmp/udp-receiver-ready").touch()
        data, _ = connection.recvfrom(32)
        Path("/tmp/udp-result").write_text(data.decode("ascii"))
elif role == "attack":
    from scapy.all import IP, UDP, Raw, send

    send(IP(src=peer[0], dst=host[0]) / UDP(sport=peer[1], dport=host[1]) / Raw(b"spoofed"), verbose=False)
elif role == "finish":
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as connection:
        connection.sendto(b"finish", ("127.0.0.1", peer[1]))
else:
    raise ValueError(f"Unsupported role: {role}")
