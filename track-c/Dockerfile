# C17 built by gcc 16 for x86-64-v3, linked statically, copied into a slim
# runtime stage. Legacy-builder syntax only (the timing box has no BuildKit).
FROM gcc:16@sha256:43978d7710a803d1c3d32f4117c1528ec7285a4ce2c0a51e982ec1a6e3d87d95 AS build
COPY join.c /src/
RUN gcc -O3 -march=x86-64-v3 -mpclmul -maes -mtune-ctrl=^avoid_false_dep_for_bmi -frename-registers -static -o /src/join /src/join.c

FROM debian:13-slim@sha256:62bd423aeee912639ca8ee681c2b762cb3e25137e0a92b20be455898a9f034ad
COPY --from=build /src/join /app/join
ENTRYPOINT ["/app/join"]
