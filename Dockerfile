# synap-node：在 musl 环境里静态编译，运行镜像只保留二进制。
# 工具链与 justfile / CI 保持一致。
ARG RUST_TOOLCHAIN=nightly-2026-02-28
ARG ALPINE_VERSION=3.22

FROM alpine:${ALPINE_VERSION} AS builder
ARG RUST_TOOLCHAIN
RUN apk add --no-cache build-base ca-certificates curl

ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:$PATH
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --profile minimal --default-toolchain "${RUST_TOOLCHAIN}" \
    && rustup target add x86_64-unknown-linux-musl

WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates

# 不用 BuildKit cache mount，这样没有 buildx 的环境也能编。
ENV CARGO_TERM_COLOR=always
RUN cargo build --release --locked -p synap-node --target x86_64-unknown-linux-musl \
    && strip target/x86_64-unknown-linux-musl/release/synap-node \
    && mkdir -p /out \
    && cp target/x86_64-unknown-linux-musl/release/synap-node /out/synap-node

FROM scratch
COPY --from=builder /out/synap-node /usr/local/bin/synap-node
ENV SYNAP_NODE_DATA_DIR=/data \
    SYNAP_NODE_LISTEN=0.0.0.0:8790
EXPOSE 8790
VOLUME ["/data"]
ENTRYPOINT ["/usr/local/bin/synap-node"]
CMD ["serve"]
