import socket, sys
d, names = sys.argv[1], sys.argv[2:]
s = socket.socket(); s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1); s.bind(("127.0.0.1", 9295)); s.listen(8); s.settimeout(60)
for name in names:
    c, _ = s.accept(); c.settimeout(5); buf = b""
    try:
        while True:
            x = c.recv(65536)
            if not x: break
            buf += x
            if b"\r\n\r\n" in buf:
                head, _, body = buf.partition(b"\r\n\r\n")
                n = [int(l.split(b":")[1]) for l in head.split(b"\r\n") if l.lower().startswith(b"content-length")]
                if n and len(body) >= n[0]: break
    except socket.timeout: pass
    open(f"{d}/{name}.bin", "wb").write(buf)
    c.sendall(b"HTTP/1.1 403 Forbidden\r\nRP-Application-Reason: 80108bff\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"); c.close()
