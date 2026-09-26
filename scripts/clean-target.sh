#!/usr/bin/env bash
# Reclaim build disk without losing a full rebuild.
#
# Long agent runs accumulate cache faster than it is reclaimed: a wave adds a
# few GiB of rlibs, and nothing ever removes them. A full volume mid-run is
# not a slowdown, it is a failure — once the disk is full the shell cannot even
# write its own output, so no command can report or fix the problem.
#
# Everything removed here is regenerable. The worst case is one cold rebuild.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MB_FREE_BEFORE="$(df -m "$ROOT" | awk 'NR==2 {print $4}')"

echo "disk before: ${MB_FREE_BEFORE} MiB free"

# A cargo target dir is pure cache. It carries CACHEDIR.TAG, so dropping it is
# safe; the cost is one rebuild.
for dir in "$ROOT/target" "$HOME/.cache/br-migration-target"; do
    [ -d "$dir" ] || continue
    size="$(/usr/bin/du -sm "$dir" 2>/dev/null | cut -f1)"
    echo "removing ${dir} (${size} MiB)"
    rm -rf "$dir"
done

# Scratch target dirs left in /tmp by other cargo-driven tools. These are the
# ones that actually grew: a single stale one reached 4.7 GiB while target/ sat
# at 10 GiB, so checking only target/ misses most of the usage.
if [ -d /private/tmp ]; then
    for dir in /private/tmp/*_target /private/tmp/*-target; do
        [ -d "$dir" ] || continue
        # Only touch things that look like a cargo target dir, not every
        # directory that happens to end in "target".
        [ -f "$dir/.rustc_info.json" ] || [ -f "$dir/CACHEDIR.TAG" ] || continue
        size="$(/usr/bin/du -sm "$dir" 2>/dev/null | cut -f1)"
        echo "removing ${dir} (${size} MiB)"
        rm -rf "$dir"
    done
fi

MB_FREE_AFTER="$(df -m "$ROOT" | awk 'NR==2 {print $4}')"
echo "disk after:  ${MB_FREE_AFTER} MiB free (freed $((MB_FREE_AFTER - MB_FREE_BEFORE)) MiB)"
