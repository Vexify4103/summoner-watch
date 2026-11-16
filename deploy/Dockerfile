FROM rust:1-bookworm AS build
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --locked --release --features relay --bin summoner-watch-relay

FROM debian:bookworm-slim
COPY --from=build /build/target/release/summoner-watch-relay /usr/local/bin/summoner-watch-relay
USER 65534:65534
ENV SUMMONER_RELAY_BIND=0.0.0.0:8787
EXPOSE 8787
ENTRYPOINT ["summoner-watch-relay"]
