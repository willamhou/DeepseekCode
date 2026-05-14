FROM rust:1-bookworm AS build

WORKDIR /workspace
COPY Cargo.toml Cargo.lock ./
COPY README.md CHANGELOG.md LICENSE ./
COPY packaging ./packaging
COPY src ./src

RUN cargo build --release --locked --bin deepseek

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates git bash \
    && rm -rf /var/lib/apt/lists/*

COPY --from=build /workspace/target/release/deepseek /usr/local/bin/deepseek

ENTRYPOINT ["deepseek"]
CMD ["--help"]
