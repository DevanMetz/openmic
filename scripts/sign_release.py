"""Sign a release executable for OpenMic's updater.

Writes <exe>.sig: a hex Ed25519 signature over the message that
src/update.rs checks (`signed_message`): the asset's file name and its
SHA-256. The private key (32-byte seed, hex) comes from the
OPENMIC_SIGNING_KEY environment variable, a GitHub Actions secret.

    python scripts/sign_release.py openmic-v0.6.2-windows-x64.exe
"""

import hashlib
import os
import sys

from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey


def main() -> None:
    exe = sys.argv[1]
    seed = os.environ.get("OPENMIC_SIGNING_KEY", "").strip()
    if len(seed) != 64:
        sys.exit("OPENMIC_SIGNING_KEY is missing or not a 32-byte hex seed")
    key = Ed25519PrivateKey.from_private_bytes(bytes.fromhex(seed))

    with open(exe, "rb") as f:
        digest = hashlib.sha256(f.read()).hexdigest()
    name = os.path.basename(exe)
    message = f"openmic-release-v1\n{name}\n{digest}\n".encode()
    signature = key.sign(message)
    key.public_key().verify(signature, message)

    with open(exe + ".sig", "w", newline="\n") as f:
        f.write(signature.hex() + "\n")
    public = key.public_key().public_bytes_raw().hex()
    print(f"signed {name} (sha256 {digest}) with key {public}")


if __name__ == "__main__":
    main()
