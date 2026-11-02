# C17 built by gcc 15 for x86-64-v3, linked statically, copied into a slim
# runtime stage. Legacy-builder syntax only (the timing box has no BuildKit).
FROM gcc:15@sha256:eb5e64286ad47187b32bda75c6b6916166ecb41da56559b8b09e0001748d2cce AS build
COPY traverse.c /src/
RUN gcc -O3 -march=x86-64-v3 -static -pthread -o /src/traverse /src/traverse.c

FROM debian:13-slim@sha256:62bd423aeee912639ca8ee681c2b762cb3e25137e0a92b20be455898a9f034ad
COPY --from=build /src/traverse /app/traverse
ENTRYPOINT ["/app/traverse"]
