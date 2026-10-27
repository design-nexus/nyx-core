#!/usr/bin/env bash
# Run the nyx-eds tests that need a real Evolution Data Server, on a private
# session bus with scratch XDG directories. Nothing touches the user's calendars.
set -euo pipefail
cd "$(dirname "$0")/.."
cargo test --locked -p nyx-eds --test session --no-run
scratch=$(mktemp -d)
trap 'fusermount3 -uz "$scratch/run/gvfs" 2>/dev/null || true; rm -rf "$scratch" 2>/dev/null || true' EXIT
export XDG_CONFIG_HOME=$scratch/config XDG_DATA_HOME=$scratch/data XDG_CACHE_HOME=$scratch/cache
export XDG_RUNTIME_DIR=$scratch/run NYX_EDS_SCRATCH=1 GSETTINGS_BACKEND=memory TZ=America/New_York
export GIO_USE_VFS=local GVFS_DISABLE_FUSE=1
mkdir -p "$XDG_CONFIG_HOME" "$XDG_DATA_HOME" "$XDG_CACHE_HOME" "$XDG_RUNTIME_DIR"
chmod 700 "$XDG_RUNTIME_DIR"
status=0
dbus-run-session -- cargo test --locked -p nyx-eds --test session -- --ignored --test-threads=1 "$@" || status=$?
exit "$status"
