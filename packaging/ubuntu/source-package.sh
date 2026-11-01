#!/bin/sh
# Builds an unsigned source package for one Ubuntu series, with all crates included because
# Launchpad builds without network access. Output: build/ppa/<series>/
set -eu

series=${1:?usage: source-package.sh <jammy|noble|resolute|stonking|tenacious>}
case "$series" in
    jammy) number=22.04; rust=1.91 ;;
    noble) number=24.04; rust=1.91 ;;
    resolute) number=26.04; rust=1.91 ;;
    stonking) number=26.10; rust=1.97 ;;
    tenacious) number=27.04; rust=1.97 ;;
    *) echo "unknown series: $series" >&2; exit 1 ;;
esac

root=$(git rev-parse --show-toplevel)
cd "$root"
upstream=$(sed -n '1s/^sccli (\(.*\)-1).*/\1/p' debian/changelog)
out="build/ppa/$series"
tree="$out/sccli-$upstream"

rm -rf "$out"
mkdir -p "$tree"
git archive HEAD | tar -x -C "$tree"
(cd "$tree" && cargo vendor --locked --quiet vendor > /dev/null)
# Ubuntu never links these Windows libraries, and they are most of the source package's size.
rm -rf "$tree"/vendor/windows_*/lib
tar -C "$out" --exclude=debian -cJf "$out/sccli_$upstream.orig.tar.xz" "sccli-$upstream"

sed -i "s/cargo-1\.91, rustc-1\.91/cargo-$rust, rustc-$rust/" "$tree/debian/control"
sed -i "1s/.*/sccli ($upstream-1~ubuntu$number.1) $series; urgency=medium/" "$tree/debian/changelog"

(cd "$tree" && dpkg-buildpackage -S -sa -d -us -uc)
rm -rf "$tree"
