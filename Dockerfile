FROM rust:1.93-slim-bookworm AS build
RUN apt-get update && apt-get install -y --no-install-recommends gcc libc6-dev pkg-config && rm -rf /var/lib/apt/lists/*
RUN rustup component add rustfmt clippy
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY tests ./tests
RUN --mount=type=cache,target=/usr/local/cargo/registry --mount=type=cache,target=/src/target \
    cargo fmt --all --check && \
    cargo clippy --locked --all-targets -- -D warnings && \
    cargo build --release --locked && \
    cargo test --locked && \
    cp target/release/witnessfs /tmp/witnessfs && \
    cargo build --release --locked --features fault-injection && \
    cp target/release/witnessfs /tmp/witnessfs-fault

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends fuse3 python3 git gcc libc6-dev ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=build /tmp/witnessfs /usr/local/bin/witnessfs
COPY --from=build /tmp/witnessfs-fault /usr/local/bin/witnessfs-fault
WORKDIR /work
CMD ["witnessfs", "--help"]
