#!/usr/bin/env python3
"""Generate rpcauth credentials for TIME Coin daemon (Bitcoin Core compatible format).

Usage:
    python3 rpcauth.py <username> [password]

If password is omitted, a random one is generated.
Output: rpcauth line to add to time.conf + the password to save securely.
"""

import sys
import os
import hmac
import hashlib
import base64

def generate_salt(length=16):
    """Generate a random hex salt."""
    return os.urandom(length).hex()

def generate_password(length=32):
    """Generate a random base64-encoded password."""
    return base64.urlsafe_b64encode(os.urandom(length)).decode('ascii').rstrip('=')

def main():
    if len(sys.argv) < 2:
        print("Usage: rpcauth.py <username> [password]", file=sys.stderr)
        print("  Generates rpcauth credentials for time.conf", file=sys.stderr)
        sys.exit(1)

    username = sys.argv[1]
    password = sys.argv[2] if len(sys.argv) > 2 else generate_password()
    salt = generate_salt()

    # HMAC-SHA256(key=salt, message=password) — matches RpcAuthenticator::check()
    h = hmac.new(salt.encode('utf-8'), password.encode('utf-8'), hashlib.sha256).hexdigest()

    print(f"\nAdd this line to your time.conf:\n")
    print(f"  rpcauth={username}:{salt}${h}\n")
    print(f"Your password (save this securely):\n")
    print(f"  {password}\n")

if __name__ == '__main__':
    main()
