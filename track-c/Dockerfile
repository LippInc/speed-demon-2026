# std-only Rust for x86-64-v3, copied into a slim runtime stage. Legacy-builder syntax only (the timing box has no BuildKit).
# +crt-static: the C runtime is linked statically, so the process starts without the dynamic loader (c-16: M -0.3 MB on box 2, T unchanged).
FROM rust:1-slim-trixie@sha256:1a939748a7cbab172de91a014e3804a97af1394a8c844faa67fce9401c4e40fb AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN RUSTFLAGS="-C target-cpu=x86-64-v3 -C target-feature=+crt-static" cargo build --release --offline --locked

FROM debian:13-slim@sha256:62bd423aeee912639ca8ee681c2b762cb3e25137e0a92b20be455898a9f034ad
COPY --from=build /src/target/release/c-join /app/c-join
ENTRYPOINT ["/app/c-join"]
