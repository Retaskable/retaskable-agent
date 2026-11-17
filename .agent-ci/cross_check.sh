#!/usr/bin/env bash
# Windows and macOS must keep compiling (CLAUDE.md: Windows-first), though CI only runs on Linux: type-check the
# whole crate, tests included, for both. `cargo check` never links, so no cross linker or SDK is needed; the only C in
# the tree is ring's build script, which gets a stand-in compiler that writes empty objects.
set -e
stub=$(mktemp -d)
trap 'rm -rf "$stub"' EXIT
cat > "$stub/cc" <<'SH'
#!/bin/sh
while [ $# -gt 0 ]; do case "$1" in -o) : > "$2"; shift ;; esac; shift; done
SH
cat > "$stub/ar" <<'SH'
#!/bin/sh
for a in "$@"; do case "$a" in *.a) : > "$a" ;; esac; done
SH
chmod +x "$stub/cc" "$stub/ar"
for target in x86_64-pc-windows-gnu aarch64-apple-darwin; do
  rustup target add "$target"
  var=$(echo "$target" | tr '-' '_')
  env "CC_$var=$stub/cc" "AR_$var=$stub/ar" cargo check --target "$target" --all-targets
done
