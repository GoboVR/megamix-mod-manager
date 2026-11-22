# Build environment for Linux release binaries (any architecture).
# Debian bookworm's glibc 2.36 keeps the binaries compatible with
# SteamOS / PB-OS and most other current distros.
FROM docker.io/library/rust:1-bookworm
RUN apt-get update \
 && apt-get install -y --no-install-recommends libudev-dev pkg-config \
 && rm -rf /var/lib/apt/lists/*
