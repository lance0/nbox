# Build nbox from source and ship it as a small runtime image.
#
# This is the from-source counterpart to `Dockerfile.release` (which only wraps
# a prebuilt binary produced by the release matrix). It exists for running the
# MCP server yourself — most usefully in multi-user pass-through mode, where one
# container serves many users and each request carries its own NetBox token:
#
#   docker build -t nbox:local .
#   docker run --rm -p 8080:8080 -v ./nbox.toml:/etc/nbox/nbox.toml:ro nbox:local \
#     --config /etc/nbox/nbox.toml serve --http 0.0.0.0:8080 --netbox-token-passthrough
#
# See docs/MCP.md for the pass-through security model and header contract.

FROM rust:1.98-bookworm AS builder
WORKDIR /src
COPY . .
RUN cargo build --release --locked --all-features

FROM debian:bookworm-slim
# CA certificates only — nbox speaks HTTPS to NetBox and nothing else.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --no-create-home --shell /usr/sbin/nologin nbox

COPY --from=builder /src/target/release/nbox /usr/local/bin/nbox

# Unprivileged: nbox needs no root, and in pass-through mode it holds no
# long-lived NetBox credential of its own that would be worth protecting with
# one — every request is authenticated by the caller's token.
USER 10001:10001
EXPOSE 8080

# nbox reads its config from `--config <path>`; pass one (see docker-compose.yml).
ENTRYPOINT ["nbox"]
CMD ["--help"]
