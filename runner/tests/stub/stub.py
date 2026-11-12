#!/usr/bin/env python3
"""Stand-ins for the services the runner image talks to, for its tests only:

- the control plane's credential exchange (POST /v1/runs/{run_id}/credentials
  and .../credentials/renew), which mints throwaway credentials;
- an S3-compatible store (PUT /<bucket>/<key>), which checks each upload's
  AWS Signature Version 4 against the credential it names and keeps the body;
- a git forge (/git/<namespace>/<name>.git), served by `git http-backend`.

GET /_stub/state returns everything the stub saw and issued, so a test can
assert on it. `stub.py --state` prints that from inside the container.

Configuration: STUB_RUN_ID, STUB_RUN_TOKEN, STUB_LIFETIME (seconds a
credential lasts, default 900), STUB_EXCHANGE_FAILS (an HTTP status the
exchange answers with after spending the token) and STUB_ENDPOINT (the storage endpoint the
exchange names, default http://stub:8080).
"""

import hashlib
import hmac
import json
import os
import re
import secrets
import subprocess
import sys
import threading
from datetime import datetime, timedelta, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.request import urlopen

PORT = 8080
FORGE = "/srv/forge"
BUCKET = "stub-bucket"
REGION = "auto"

if sys.argv[1:] == ["--state"]:
    with urlopen(f"http://127.0.0.1:{PORT}/_stub/state", timeout=5) as reply:
        print(reply.read().decode())
    sys.exit(0)

RUN_ID = os.environ["STUB_RUN_ID"]
RUN_TOKEN = os.environ["STUB_RUN_TOKEN"]
LIFETIME = int(os.environ.get("STUB_LIFETIME", "900"))
# When set, the exchange spends the token and then answers with this status,
# as a service that failed after spending it would.
EXCHANGE_FAILS = int(os.environ.get("STUB_EXCHANGE_FAILS", "0"))
ENDPOINT = os.environ.get("STUB_ENDPOINT", f"http://stub:{PORT}")
PREFIX = f"sub_test/runs/{RUN_ID}/"

with open(os.path.join(FORGE, "commits.json")) as f:
    COMMITS = json.load(f)

lock = threading.Lock()
state = {
    "run_token": RUN_TOKEN,
    "commits": COMMITS,
    "exchange_posts": 0,
    "exchanges": 0,
    "exchange_refusals": 0,
    "renewals": 0,
    "renewal_refusals": 0,
    "issued": [],
    "renewal_tokens": [],
    "puts": [],
    "rejected_puts": [],
    "objects": {},
}
credentials = {}  # access key id -> (secret, session token, expiry)
current = {"renewal_token": None, "token_spent": False}

AUTH_RE = re.compile(
    r"AWS4-HMAC-SHA256 Credential=([^/]+)/(\d{8})/([^/]+)/s3/aws4_request,\s*"
    r"SignedHeaders=([a-z0-9;-]+),\s*Signature=([0-9a-f]{64})$"
)


def now():
    return datetime.now(timezone.utc)


def mint():
    akid = "AKSTUB" + secrets.token_hex(8).upper()
    secret = secrets.token_urlsafe(30)
    session = secrets.token_urlsafe(48)
    expires = now() + timedelta(seconds=LIFETIME)
    credentials[akid] = (secret, session, expires)
    state["issued"].append(
        {"access_key_id": akid, "secret_access_key": secret, "session_token": session}
    )
    return {
        "access_key_id": akid,
        "secret_access_key": secret,
        "session_token": session,
        "expires_at": expires.strftime("%Y-%m-%dT%H:%M:%SZ"),
    }


def new_renewal_token():
    token = secrets.token_urlsafe(32)
    current["renewal_token"] = token
    state["renewal_tokens"].append(token)
    return token


def sha256_hex(data):
    return hashlib.sha256(data).hexdigest()


def sign(key, msg):
    return hmac.new(key, msg.encode(), hashlib.sha256).digest()


def check_sigv4(method, path, headers, body):
    """None when the request is signed by a live credential; else why not."""
    match = AUTH_RE.match(headers.get("Authorization", ""))
    if not match:
        return "no SigV4 Authorization header"
    akid, day, region, signed, signature = match.groups()
    if akid not in credentials:
        return "unknown access key"
    secret, session, expires = credentials[akid]
    if expires < now():
        return "expired credential"
    if region != REGION:
        return f"signed for region {region}"
    if headers.get("x-amz-security-token") != session:
        return "missing or wrong session token"
    payload_hash = headers.get("x-amz-content-sha256", "")
    if payload_hash != sha256_hex(body):
        return "x-amz-content-sha256 does not match the body"
    names = signed.split(";")
    for required in ("host", "x-amz-date", "x-amz-content-sha256", "x-amz-security-token"):
        if required not in names:
            return f"{required} is not signed"
    canonical_headers = "".join(
        f"{name}:{' '.join((headers.get(name) or '').split())}\n" for name in names
    )
    canonical_path, _, query = path.partition("?")
    canonical_request = "\n".join(
        [method, canonical_path, query, canonical_headers, signed, payload_hash]
    )
    scope = f"{day}/{region}/s3/aws4_request"
    to_sign = "\n".join(
        ["AWS4-HMAC-SHA256", headers.get("x-amz-date", ""), scope, sha256_hex(canonical_request.encode())]
    )
    key = sign(("AWS4" + secret).encode(), day)
    for part in (region, "s3", "aws4_request"):
        key = sign(key, part)
    expected = hmac.new(key, to_sign.encode(), hashlib.sha256).hexdigest()
    if not hmac.compare_digest(expected, signature):
        return "signature does not match"
    return None


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt, *args):
        sys.stderr.write("stub: " + (fmt % args) + "\n")

    def body(self):
        length = int(self.headers.get("Content-Length") or 0)
        return self.rfile.read(length) if length else b""

    def reply(self, status, payload=b"", content_type="application/json"):
        if isinstance(payload, (dict, list)):
            payload = json.dumps(payload).encode()
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def bearer(self):
        value = self.headers.get("Authorization", "")
        return value[len("Bearer "):] if value.startswith("Bearer ") else None

    def do_GET(self):
        if self.path == "/_stub/state":
            with lock:
                return self.reply(200, state)
        if self.path.startswith("/git/"):
            return self.git()
        self.reply(404, {"error": "not found"})

    def do_POST(self):
        if self.path.startswith("/git/"):
            return self.git()
        body = self.body()
        del body
        with lock:
            if self.path == f"/v1/runs/{RUN_ID}/credentials":
                state["exchange_posts"] += 1
                if self.bearer() != RUN_TOKEN or current["token_spent"]:
                    state["exchange_refusals"] += 1
                    return self.reply(401, {"error": "token refused"})
                current["token_spent"] = True
                if EXCHANGE_FAILS:
                    return self.reply(EXCHANGE_FAILS, {"error": "failed after spending the token"})
                state["exchanges"] += 1
                return self.reply(
                    200,
                    {
                        "credential": mint(),
                        "renewal_token": new_renewal_token(),
                        "storage": {"endpoint": ENDPOINT, "bucket": BUCKET, "region": REGION},
                        "run_log": {"prefix": PREFIX},
                    },
                )
            if self.path == f"/v1/runs/{RUN_ID}/credentials/renew":
                if current["renewal_token"] is None or self.bearer() != current["renewal_token"]:
                    state["renewal_refusals"] += 1
                    return self.reply(401, {"error": "renewal refused"})
                state["renewals"] += 1
                return self.reply(200, {"credential": mint()})
        self.reply(404, {"error": "not found"})

    def do_PUT(self):
        body = self.body()
        with lock:
            prefix = f"/{BUCKET}/"
            if not self.path.startswith(prefix):
                return self.reply(404, b"no such bucket", "text/plain")
            why = check_sigv4("PUT", self.path, self.headers, body)
            key = self.path[len(prefix):]
            if why:
                state["rejected_puts"].append({"key": key, "why": why})
                return self.reply(403, why.encode(), "text/plain")
            match = AUTH_RE.match(self.headers["Authorization"])
            state["puts"].append({"key": key, "access_key_id": match.group(1)})
            state["objects"][key] = body.decode("utf-8", "replace")
        self.send_response(200)
        self.send_header("ETag", '"' + hashlib.md5(body).hexdigest() + '"')
        self.send_header("Content-Length", "0")
        self.end_headers()

    def git(self):
        path, _, query = self.path.partition("?")
        env = {
            "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
            "GIT_PROJECT_ROOT": FORGE,
            "GIT_HTTP_EXPORT_ALL": "1",
            "PATH_INFO": path[len("/git"):],
            "QUERY_STRING": query,
            "REQUEST_METHOD": self.command,
            "CONTENT_TYPE": self.headers.get("Content-Type", ""),
            "REMOTE_ADDR": self.client_address[0],
            "GIT_CONFIG_COUNT": "1",
            "GIT_CONFIG_KEY_0": "safe.directory",
            "GIT_CONFIG_VALUE_0": "*",
        }
        if self.headers.get("Git-Protocol"):
            env["GIT_PROTOCOL"] = self.headers["Git-Protocol"]
        if self.headers.get("Content-Encoding"):
            env["HTTP_CONTENT_ENCODING"] = self.headers["Content-Encoding"]
        body = self.body() if self.command == "POST" else b""
        if body:
            env["CONTENT_LENGTH"] = str(len(body))
        out = subprocess.run(
            ["git", "http-backend"], input=body, env=env, capture_output=True, check=False
        ).stdout
        sep = b"\r\n\r\n" if b"\r\n\r\n" in out else b"\n\n"
        head, _, payload = out.partition(sep)
        status, headers = 200, []
        for line in head.decode("latin-1").splitlines():
            name, _, value = line.partition(":")
            if name.lower() == "status":
                status = int(value.split()[0])
            elif name:
                headers.append((name, value.strip()))
        self.send_response(status)
        for name, value in headers:
            if name.lower() != "content-length":
                self.send_header(name, value)
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)


if __name__ == "__main__":
    ThreadingHTTPServer(("0.0.0.0", PORT), Handler).serve_forever()
