# balq as a sidecar next to your node.
#
#   docker build -t balq .
#   docker run -v balq-data:/data balq index 0x… --rpc http://node:8545
#   # watches it, backfills to the deploy, follows; or mount a balq.toml and run `balq --config /data/balq.toml index`
#
# The archive lives in the /data volume as /data/balq.redb.

FROM rust:1.90-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates ./crates
RUN cargo build --release --locked -p balq

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --create-home balq
COPY --from=build /src/target/release/balq /usr/local/bin/balq
USER balq
VOLUME /data
WORKDIR /data
ENTRYPOINT ["balq", "--data", "/data/balq.redb"]
CMD ["status"]
