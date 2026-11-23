FROM rust:1-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked

# distroless/cc ships glibc, CA certificates (for https CalDAV) and tzdata (for TZ).
FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=build /src/target/release/calmcp /usr/local/bin/calmcp
ENV RUST_LOG=info
# Default port; override with PORT.
EXPOSE 8000
ENTRYPOINT ["/usr/local/bin/calmcp"]
