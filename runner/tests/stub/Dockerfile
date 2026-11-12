# The runner image's test stub: credential exchange, storage and forge in one
# process (stub.py). Build it with runner/tests as the context; run.sh does.
ARG BASE_IMAGE=debian:trixie-slim@sha256:a72b4433de1cf7d6bae0c4cee74d0547f58601a643b3375f070c51600d63b6b2
FROM ${BASE_IMAGE}
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates git python3 \
 && rm -rf /var/lib/apt/lists/*
COPY protocols /protocols
COPY stub/make-forge.sh /stub/make-forge.sh
RUN sh /stub/make-forge.sh /protocols /srv/forge
COPY stub/stub.py /stub/stub.py
EXPOSE 8080
CMD ["python3", "-I", "/stub/stub.py"]
