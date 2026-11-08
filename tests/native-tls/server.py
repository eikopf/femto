"""One loopback TLS connection using Python's OpenSSL backend."""
import os
import socket
import ssl
import sys
ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
ctx.minimum_version = ctx.maximum_version = ssl.TLSVersion.TLSv1_2
ctx.load_cert_chain(sys.argv[1], sys.argv[2])
listener = socket.socket()
listener.bind(("127.0.0.1", 0))
listener.listen(1)
listener.settimeout(10)
print(listener.getsockname()[1], flush=True)
try:
    connection, _ = listener.accept()
    connection.settimeout(5)
    with ctx.wrap_socket(connection, server_side=True) as stream:
        request = b""
        while len(request) < 4096 and not request.endswith(b"\r\n\r\n"):
            chunk = stream.recv(4096 - len(request))
            if not chunk:
                break
            request += chunk
        if request == b"GET / HTTP/1.1\r\nHost: peer.example\r\n\r\n":
            stream.sendall(b"hello world" * 2000)
            if sys.argv[3] == "truncated":
                os.close(stream.detach())
            else:
                stream.unwrap().close()
except (OSError, ssl.SSLError):
    pass
finally:
    listener.close()
