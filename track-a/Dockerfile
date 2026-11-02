FROM rust:1-slim-trixie@sha256:1a939748a7cbab172de91a014e3804a97af1394a8c844faa67fce9401c4e40fb AS build
COPY . /src
# Static C runtime (probe a-26): no dynamic loader before main. alderlake is the scoring box's i5-13500 (AVX2, no AVX-512).
RUN cd /src && RUSTFLAGS="-C target-cpu=alderlake -C relocation-model=static -C target-feature=+crt-static,+aes,+vaes" cargo build --release --offline --locked
ENTRYPOINT ["/src/target/release/sd-a"]
