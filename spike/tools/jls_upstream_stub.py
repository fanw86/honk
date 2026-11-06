#!/usr/bin/env python3
"""Dummy JLS upstream for the Phase 0 spike.

The shadowquic server requires a `jls-upstream` address. Any TCP listener
works: non-JLS (probe) ClientHellos are relayed there, and a forwarded probe
must not be able to complete a JLS-authenticated handshake. This stub accepts
connections and holds them open without speaking TLS, so a forwarded probe
stalls instead of authenticating.

Usage: jls_upstream_stub.py [--listen 127.0.0.1:14431]
"""

import argparse
import socket
import threading


def serve(addr):
    host, port = addr
    listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    listener.bind((host, port))
    listener.listen(64)
    print("jls-upstream stub listening on %s:%d" % (host, port), flush=True)
    while True:
        conn, peer = listener.accept()
        print("jls-upstream stub: accepted %s:%d" % peer, flush=True)
        threading.Thread(target=hold, args=(conn,), daemon=True).start()


def hold(conn):
    # Drain and discard; never write TLS, never close early.
    try:
        while conn.recv(4096):
            pass
    except OSError:
        pass
    finally:
        conn.close()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--listen", default="127.0.0.1:14431")
    args = parser.parse_args()
    host, _, port = args.listen.rpartition(":")
    serve((host or "127.0.0.1", int(port)))


if __name__ == "__main__":
    main()
