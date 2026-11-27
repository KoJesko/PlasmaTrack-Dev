#!/usr/bin/env bash
# Builds the Linux release package (docs/UPDATES.md, "Linux"):
#   installer/dist/linux-<version>/PlasmaTrack-<version>-linux-x86_64.tar.gz
#
#   scripts/package-linux.sh [--dll PATH] [--force]
#
# The plugin is the Windows release's DLL (installer/payload, from `npm run
# payload`), so Proton players run the same bytes as Windows players.
#
# The client is built in a container on an old distribution, so it runs on
# players' PCs: a binary built on a rolling distribution needs its newest glibc.
#   PT_BUILD_IMAGE   image with a C compiler (default buildpack-deps:bullseye);
#                    "host" builds without a container
#   PT_MAX_GLIBC     newest glibc the binary may need (default 2.35)
# The host's Rust toolchain is mounted into the container.
set -euo pipefail

die() { printf 'error: %s\n' "$*" >&2; exit 1; }

root=$(cd "$(dirname "$0")/.." && pwd)
dll=$root/installer/payload/plasmatrack.dll
force=0
while (($#)); do
    case $1 in
        --dll) dll=$(realpath "${2:?--dll needs a path}"); shift 2 ;;
        --force) force=1; shift ;;
        *) die "usage: $0 [--dll PATH] [--force]" ;;
    esac
done
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/crates/pt-client/Cargo.toml" | head -1)
name=PlasmaTrack-$version-linux-x86_64
out=$root/installer/dist/linux-$version
image=${PT_BUILD_IMAGE:-buildpack-deps:bullseye}
max_glibc=${PT_MAX_GLIBC:-2.35}
target=$root/target/linux-package

# Publishing registers the package's hashes, so a published package is not
# rebuilt by accident.
[[ ! -e $out/$name.tar.gz || $force == 1 ]] || die "$out/$name.tar.gz exists. If it was published, bump the version; otherwise use --force"
[[ -f $dll ]] || die "no $dll: copy the Windows release's installer/payload here, or give --dll"
update=$root/installer/dist/update-$version/update.json
if [[ -f $update ]]; then
    grep -q "$(sha256sum "$dll" | cut -d' ' -f1)" "$update" || die "$dll is not the plugin of the Windows $version release ($update)"
else
    echo "warning: no $update here; make sure $dll is the Windows $version release's plugin" >&2
fi

cargo_home=${CARGO_HOME:-$HOME/.cargo}
sysroot=$(cd "$root" && rustc --print sysroot)
# Build paths would otherwise put the builder's home folder in the binary.
flags="--remap-path-prefix=$cargo_home=/cargo --remap-path-prefix=$root=/plasmatrack"
build=(cargo build --release --locked -p pt-client --bin plasmatrack-client)
if [[ $image == host ]]; then
    (cd "$root" && RUSTFLAGS=$flags CARGO_TARGET_DIR=$target "${build[@]}")
else
    mkdir -p "$target" "$cargo_home/registry" "$cargo_home/git"
    docker run --rm --user "$(id -u):$(id -g)" \
        -v "$root:$root:ro" -v "$target:$target" -v "$sysroot:/rust:ro" \
        -v "$cargo_home/registry:/cargo-home/registry" -v "$cargo_home/git:/cargo-home/git" \
        -e HOME=/tmp -e CARGO_HOME=/cargo-home -e CARGO_TARGET_DIR="$target" \
        -e RUSTFLAGS="--remap-path-prefix=/cargo-home=/cargo --remap-path-prefix=$root=/plasmatrack" \
        -e PATH=/rust/bin:/usr/local/bin:/usr/bin:/bin \
        -w "$root" "$image" "${build[@]}"
fi

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
pkg=$stage/$name
mkdir -p "$pkg/licenses"
install -m755 "$target/release/plasmatrack-client" "$pkg/"
strip "$pkg/plasmatrack-client"

needs=$(objdump -T "$pkg/plasmatrack-client" | grep -o 'GLIBC_[0-9.]*' | sed 's/GLIBC_//' | sort -Vu | tail -1)
[[ $(printf '%s\n%s\n' "$needs" "$max_glibc" | sort -V | tail -1) == "$max_glibc" ]] ||
    die "the client needs glibc $needs, newer than $max_glibc: build in an older image (PT_BUILD_IMAGE)"

install -m644 "$dll" "$pkg/plasmatrack.dll"
install -m644 "$root/config/plasmatrack.sample.toml" "$root/LICENSE.md" "$root/THIRD_PARTY_NOTICES.md" "$pkg/"
install -m644 "$root/third_party/scssdk-1.14/sdk_license.txt" "$pkg/licenses/scssdk-1.14-LICENSE.txt"
install -m644 "$root/third_party/truckersmp-client-sdk-1.1.0/LICENSE" "$pkg/licenses/truckersmp-client-sdk-LICENSE.txt"
install -m644 "$root"/third_party/notices/* "$pkg/licenses/"
install -m755 "$root/packaging/linux/install.sh" "$root/packaging/linux/uninstall.sh" "$pkg/"
install -m644 "$root/packaging/linux/plasmatrack-client.service" "$root/packaging/linux/README.txt" "$pkg/"

mkdir -p "$out"
tar -C "$stage" --owner=0 --group=0 --numeric-owner -czf "$out/$name.tar.gz" "$name"
echo "built $out/$name.tar.gz (needs glibc $needs)"
(cd "$pkg" && sha256sum plasmatrack-client plasmatrack.dll)
