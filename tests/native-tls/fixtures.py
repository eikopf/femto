"""Generate short-lived, local-only certificate fixtures."""
import datetime
import pathlib
import sys
from cryptography import x509
from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import rsa
from cryptography.x509.oid import NameOID
root = pathlib.Path(sys.argv[1])
now = datetime.datetime.now(datetime.timezone.utc)
key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
name = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, "Femto test root")])
ca = (x509.CertificateBuilder().subject_name(name).issuer_name(name).public_key(key.public_key())
      .serial_number(x509.random_serial_number()).not_valid_before(now-datetime.timedelta(days=1))
      .not_valid_after(now+datetime.timedelta(days=2)).add_extension(x509.BasicConstraints(ca=True, path_length=0), critical=True)
      .sign(key, hashes.SHA256()))
(root / "ca.pem").write_bytes(ca.public_bytes(serialization.Encoding.PEM))
for label, expired, other in [("valid", False, False), ("expired", True, False), ("untrusted", False, True)]:
    server_key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
    subject = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, "peer.example")])
    signer = server_key if other else key
    issuer = subject if other else name
    cert = (x509.CertificateBuilder().subject_name(subject).issuer_name(issuer).public_key(server_key.public_key())
            .serial_number(x509.random_serial_number()).not_valid_before(now-datetime.timedelta(days=2))
            .not_valid_after(now+datetime.timedelta(days=-1 if expired else 1))
            .add_extension(x509.SubjectAlternativeName([x509.DNSName("peer.example")]), critical=False)
            .sign(signer, hashes.SHA256()))
    (root / (label+".pem")).write_bytes(cert.public_bytes(serialization.Encoding.PEM))
    path = root / (label+".key")
    path.write_bytes(server_key.private_bytes(serialization.Encoding.PEM, serialization.PrivateFormat.PKCS8, serialization.NoEncryption()))
    path.chmod(0o600)
