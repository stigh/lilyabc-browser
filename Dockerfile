# syntax=docker/dockerfile:1
#
# Multi-stage build for lilyabc-browser.
#   * builder  — pinned modern Rust toolchain + egui build deps; compiles the binary.
#   * runtime  — slim image with the engravers (lilypond, abcm2ps) and the shared
#                libraries the GUI dlopen's at runtime.
#
# Compile-check only (fast, no runtime-stage package risk):
#     docker build --target builder -t lilyabc-build .
# Full image:
#     docker build -t lilyabc-browser .
# Run with the host display forwarded (Wayland or XWayland) — see README.

# ---------- build stage ----------
FROM rust:1-bookworm AS builder
WORKDIR /app

# Build-time deps for egui/eframe (winit + xkb + fontconfig). Most GL/X11/Wayland
# libraries are dlopen'd at runtime, so they are only needed in the runtime stage.
RUN apt-get update && apt-get install -y --no-install-recommends \
        pkg-config \
        libfontconfig1-dev \
        libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev \
        libxkbcommon-dev \
        libwayland-dev \
    && rm -rf /var/lib/apt/lists/*

# Cache the dependency build: compile a stub against the manifest first, so editing
# our own sources doesn't re-download/re-compile the whole dependency tree.
COPY Cargo.toml ./
COPY Cargo.lock* ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs \
    && cargo build --release \
    && rm -rf src

# Real sources.
COPY src ./src
RUN touch src/main.rs && cargo build --release

# ---------- runtime stage ----------
FROM debian:bookworm-slim AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends \
        lilypond \
        abcm2ps \
        librsvg2-bin \
        libfontconfig1 \
        libxcb1 libxcb-render0 libxcb-shape0 libxcb-xfixes0 \
        libxkbcommon0 libxkbcommon-x11-0 \
        libwayland-client0 libwayland-egl1 libwayland-cursor0 \
        libgl1 libegl1 \
        ca-certificates fonts-dejavu-core \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/lilyabc-browser /usr/local/bin/lilyabc-browser
ENTRYPOINT ["/usr/local/bin/lilyabc-browser"]
