FROM docker.io/clux/muslrust:stable as cargo-build
WORKDIR /usr/src/cowdexsolver

# Copy and Build Code
COPY . .
RUN env CARGO_PROFILE_RELEASE_DEBUG=1 cargo build --target x86_64-unknown-linux-musl --release

RUN \
    cd .. && \
    git clone https://github.com/gnosis/regex-stream-split.git && \
    cd regex-stream-split && \
    git checkout edc88224612b9e151c334fa6d3a7d20575d83836 && \
    cargo build --target x86_64-unknown-linux-musl --release

# Extract Binary
FROM docker.io/alpine:latest

# Handle signal handlers properly
RUN apk add --no-cache tini
COPY --from=cargo-build /usr/src/regex-stream-split/target/x86_64-unknown-linux-musl/release/regex-stream-split /usr/local/bin/regex-stream-split
COPY --from=cargo-build /usr/src/cowdexsolver/target/x86_64-unknown-linux-musl/release/cowdexsolver /usr/local/bin/cowdexsolver
COPY docker/startup.sh /usr/local/bin/startup.sh
COPY data/token_list_for_buffer_trading.json /data/token_list_for_buffer_trading.json

CMD echo "Specify binary"
ENTRYPOINT ["/sbin/tini", "--", "startup.sh"]