#!/usr/bin/env bash
set -euo pipefail

source_dir=$(cd "$1" && pwd)
lazyfs="$source_dir/lazyfs/build/lazyfs"
export LD_LIBRARY_PATH="$source_dir/libs/libpcache/build:$source_dir/libs/libpcache/build/_deps/spdlog-build${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
work=$(mkdir -p "$2" && cd "$2" && pwd)
expect=${3:-pass}

root="$work/root"
mount="$work/mount"
faults="$work/faults.fifo"
done_fifo="$work/done.fifo"
config="$work/lazyfs.toml"

unmount() {
  if mountpoint -q "$mount"; then
    fusermount3 -u "$mount" || fusermount3 -uz "$mount"
  fi
}

stop() {
  unmount
  if [ -n "${pid:-}" ]; then
    wait "$pid" || true
  fi
}
trap stop EXIT

unmount
rm -rf "$root" "$mount" "$faults" "$done_fifo"
mkdir -p "$root" "$mount"

cat > "$config" <<TOML
[faults]
fifo_path="$faults"
fifo_path_completed="$done_fifo"

[cache]
apply_eviction=false

[cache.simple]
custom_size="1gb"
blocks_per_page=1

[filesystem]
log_all_operations=false
logfile="$work/lazyfs.log"
TOML

"$lazyfs" "$mount" --config-path "$config" -o modules=subdir -o subdir="$root" -s -f &
pid=$!

for _ in $(seq 1 100); do
  if ! kill -0 "$pid" 2>/dev/null; then
    echo "LazyFS exited before mounting $mount" >&2
    exit 1
  fi
  if mountpoint -q "$mount" && [ -p "$done_fifo" ]; then
    break
  fi
  sleep 0.1
done
if ! mountpoint -q "$mount"; then
  echo "LazyFS did not mount $mount" >&2
  exit 1
fi

probe="$mount/probe"
printf 'unsynced' > "$probe.unsynced"
printf 'synced' > "$probe.synced"
sync -d "$probe.synced"
exec 3< "$done_fifo"
printf 'lazyfs::clear-cache\n' > "$faults"
read -r -u 3 reply
exec 3<&-
if [ "$(cat "$probe.synced")" != "synced" ]; then
  echo "a synced file lost its data under LazyFS" >&2
  exit 1
fi
if [ "$(cat "$probe.unsynced")" = "unsynced" ]; then
  echo "LazyFS kept unsynced data after clear-cache" >&2
  exit 1
fi
rm -f "$probe.synced" "$probe.unsynced"
echo "LazyFS drops unsynced data ($reply)"

export PLAINDB_POWERLOSS_DIR="$mount"
export PLAINDB_POWERLOSS_FIFO="$faults"
export PLAINDB_POWERLOSS_DONE_FIFO="$done_fifo"
if [ "$expect" = "loss" ]; then
  export PLAINDB_POWERLOSS_SKIP_FLUSH=1
  if cargo test --locked --all-features -p plaindb-core --test powerloss -- --ignored --nocapture; then
    echo "the power-loss test passed with every flush skipped" >&2
    exit 1
  fi
  echo "the power-loss test fails when flushes are skipped"
else
  cargo test --locked --all-features -p plaindb-core --test powerloss -- --ignored --nocapture
fi
