# syntax=docker/dockerfile:1.27
#
# seerr-mcp image: cargo-chef build, then distroless. Every dependency is on crates.io, so no stage
# needs a secret.
#
# Runtime is gcr.io/distroless/cc-debian12:nonroot: glibc, libgcc and the CA bundle, no shell. So
# there is NO healthcheck (a distroless image has nothing to run one with); the scrape of /metrics is
# the liveness signal, and /healthz is there for a human with curl.
#
# GLIBC MUST MATCH THE RUNTIME. cc-debian12 is bookworm (glibc 2.36), so the builder is pinned to the
# `-bookworm` cargo-chef tag; the unsuffixed one floats to a newer Debian and links GLIBC_2.39.
#
# CROSS-COMPILE, NOT EMULATE: the build stages run on the build machine's arch and target
# TARGETARCH, so an x64 runner builds the arm64 image at native speed. The runtime stage runs nothing.
FROM --platform=$BUILDPLATFORM lukemathwalker/cargo-chef:latest-rust-1-bookworm AS chef
WORKDIR /build
ENV CARGO_HOME=/usr/local/cargo
# The pinned toolchain must be in place BEFORE `rustup target add`, or the target goes to the image's
# default toolchain and the final build (which then sees this file) has no std for it: E0463 on the
# cross leg only, since a native build needs no extra target. v1.0.0's image failed exactly so.
COPY rust-toolchain.toml .

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
ARG TARGETARCH
ARG BUILDARCH
# No libssl: the lock has no openssl-sys (tests/tls_is_rustls.rs). The C that is built is rustls's
# crypto provider; the cc crate finds `<prefix>-gcc` itself, rustc's linker must be named.
# libc6-dev-<arch>-cross is only a Recommends of the gcc package, so it is listed explicitly.
RUN set -eu; \
    case "$TARGETARCH" in \
        amd64) triple=x86_64-unknown-linux-gnu;  prefix=x86_64-linux-gnu;  gcc=gcc-x86-64-linux-gnu ;; \
        arm64) triple=aarch64-unknown-linux-gnu; prefix=aarch64-linux-gnu; gcc=gcc-aarch64-linux-gnu ;; \
        *) echo "unsupported TARGETARCH: $TARGETARCH" >&2; exit 1 ;; \
    esac; \
    echo "$triple" > /target-triple; \
    echo "$prefix" > /target-prefix; \
    rustup target add "$triple"; \
    if [ "$TARGETARCH" = "$BUILDARCH" ]; then \
        pkgs="ca-certificates"; \
    else \
        pkgs="ca-certificates $gcc libc6-dev-${TARGETARCH}-cross"; \
        printf '[target.%s]\nlinker = "%s-gcc"\n' "$triple" "$prefix" >> "$CARGO_HOME/config.toml"; \
    fi; \
    apt-get update && apt-get install -y --no-install-recommends $pkgs \
    && rm -rf /var/lib/apt/lists/*
COPY --from=planner /build/recipe.json recipe.json
RUN cargo chef cook --release --target "$(cat /target-triple)" --recipe-path recipe.json
COPY . .
RUN cargo build --release --locked --target "$(cat /target-triple)" --bin seerr-mcp && \
    cp "target/$(cat /target-triple)/release/seerr-mcp" /seerr-mcp && \
    "$(cat /target-prefix)-strip" /seerr-mcp

FROM gcr.io/distroless/cc-debian12:nonroot AS runtime
ARG VERSION=0.0.0
ARG GIT_SHA=dev
LABEL org.opencontainers.image.title="seerr-mcp" \
      org.opencontainers.image.version="${VERSION}" \
      org.opencontainers.image.revision="${GIT_SHA}" \
      org.opencontainers.image.source="https://github.com/dlepaux/seerr-mcp" \
      org.opencontainers.image.licenses="MIT"
COPY --from=builder /seerr-mcp /usr/local/bin/seerr-mcp
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/seerr-mcp"]
