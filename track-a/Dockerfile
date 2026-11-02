FROM rust:1-slim-trixie@sha256:1a939748a7cbab172de91a014e3804a97af1394a8c844faa67fce9401c4e40fb AS build
COPY . /src
# Static C runtime (probe a-26): no dynamic loader before main; RUSTFLAGS replaces .cargo/config.toml, so it repeats target-cpu.
RUN cd /src && RUSTFLAGS="-C target-cpu=x86-64-v3 -C target-feature=+crt-static,+aes,+vaes" cargo build --release --offline --locked

FROM debian:13-slim@sha256:62bd423aeee912639ca8ee681c2b762cb3e25137e0a92b20be455898a9f034ad
COPY --from=build --chmod=755 /src/target/release/sd-a /app/sd-a
ENTRYPOINT ["/app/sd-a"]
