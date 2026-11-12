# The hosted runner: one run of one pinned Protocol, then exit. Build it from the
# repository root:
#
#   docker build --platform linux/arm64 -f runner/Dockerfile -t arc-runner .
#
# docs/ARCHITECTURE.md ("The runner image") describes the inputs, the credential
# exchange and the run log.

ARG RUST_IMAGE=rust:1.99.1-slim-trixie@sha256:f6c4a35d21dbb3c89d7a8d056c193e94fc09037f2b58dd858b09c39a47dd02d7
ARG BASE_IMAGE=debian:trixie-slim@sha256:a72b4433de1cf7d6bae0c4cee74d0547f58601a643b3375f070c51600d63b6b2

# ---- build: arc, linked against DuckDB's release library --------------------
FROM --platform=linux/arm64 ${RUST_IMAGE} AS build

# The DuckDB release arc links and runs SQL steps with: the shared library and
# the command line, each checked against the digest GitHub publishes for the
# release asset. One version throughout, the same pin as CI's DUCKDB_VERSION.
ARG DUCKDB_VERSION=v2.0.0
ARG DUCKDB_LIBS_SHA256=c60eecf91143ca6bbac55c4f4d40dc22520488b0ac257c86e0fe9a2b451cf2b7
ARG DUCKDB_CLI_SHA256=859e1875d2a9515fbb4bf8871061437a14af82320da68c753f094c4b776348fe
# Parallel compile jobs; keep it low on a small machine.
ARG CARGO_BUILD_JOBS=4

RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl \
 && rm -rf /var/lib/apt/lists/*

WORKDIR /duckdb
RUN set -eu; \
    base="https://github.com/duckdb/duckdb/releases/download/${DUCKDB_VERSION}"; \
    curl -fsSL -o libs.tar.gz "$base/duckdb-shared-libs-linux-arm64.tar.gz"; \
    echo "${DUCKDB_LIBS_SHA256}  libs.tar.gz" | sha256sum -c -; \
    curl -fsSL -o cli.tar.gz "$base/duckdb-cli-linux-arm64.tar.gz"; \
    echo "${DUCKDB_CLI_SHA256}  cli.tar.gz" | sha256sum -c -; \
    tar -xzf libs.tar.gz; tar -xzf cli.tar.gz; rm libs.tar.gz cli.tar.gz; \
    chmod 0755 duckdb; test -f libduckdb.so

ENV DUCKDB_LIB_DIR=/duckdb DUCKDB_INCLUDE_DIR=/duckdb LD_LIBRARY_PATH=/duckdb

WORKDIR /src
COPY Cargo.toml Cargo.lock build.rs ./
COPY src src
COPY skills skills
COPY operators operators
COPY vendor vendor
COPY tests/fixtures tests/fixtures
RUN cargo build --release --locked --bin arc --jobs "${CARGO_BUILD_JOBS}" \
 && install -m 0755 target/release/arc /usr/local/bin/arc \
 && /usr/local/bin/arc --version \
 && rm -rf target

# ---- runtime ----------------------------------------------------------------
FROM --platform=linux/arm64 ${BASE_IMAGE}

# curl exchanges and renews the credential and uploads the run log (signing with
# --aws-sigv4), jq reads the exchange's replies, git fetches the pinned commit,
# and setpriv (util-linux) runs the Protocol as an unprivileged user.
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl git jq util-linux \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --create-home --uid 10001 --user-group --shell /usr/sbin/nologin protocol

COPY --from=build /duckdb/libduckdb.so /usr/local/lib/libduckdb.so
COPY --from=build /duckdb/duckdb /usr/local/bin/duckdb
COPY --from=build /usr/local/bin/arc /usr/local/bin/arc
RUN ldconfig && arc --version && duckdb --version && command -v setpriv
COPY runner/entrypoint.sh /usr/local/bin/arc-runner

# The entrypoint starts as root so the credential it holds is out of the
# Protocol's reach; it runs every Protocol step as `protocol`.
ENTRYPOINT ["/usr/local/bin/arc-runner"]
