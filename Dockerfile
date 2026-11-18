FROM node:22-bookworm-slim AS node

FROM rust:1.99-bookworm AS tools
RUN apt-get update && apt-get install -y --no-install-recommends \
    libwebkit2gtk-4.1-dev libxdo-dev libssl-dev libudev-dev \
    libayatana-appindicator3-dev librsvg2-dev patchelf \
    && rm -rf /var/lib/apt/lists/*
COPY --from=node /usr/local/bin/node /usr/local/bin/node
RUN useradd --create-home --uid 1000 builder \
    && mkdir -p /build /workspace \
    && chown builder:builder /build /workspace
ENV CARGO_TARGET_DIR=/build/target
WORKDIR /workspace

FROM tools AS windows-tools
RUN apt-get update && apt-get install -y --no-install-recommends clang lld llvm \
    && rm -rf /var/lib/apt/lists/*
RUN rustup target add x86_64-pc-windows-msvc \
    && cargo install --locked cargo-xwin --version 0.23.1
ENV XWIN_CACHE_DIR=/build/xwin

FROM tools AS linux
COPY --chown=builder:builder . .
ENV CARGO_HOME=/build/cargo
USER builder
RUN mkdir -p target/tmp

FROM windows-tools AS windows
COPY --chown=builder:builder . .
ENV CARGO_HOME=/build/cargo
USER builder
RUN mkdir -p target/tmp
