#!/bin/sh
set -eu

repo="rahulunair/xfer"
asset="xfer-x86_64-linux-gnu.tar.gz"
version="${XFER_VERSION:-latest}"
install_dir="${XFER_INSTALL_DIR:-${HOME:?HOME is not set}/.local/bin}"

case "$(uname -s)" in
    Linux) ;;
    *)
        printf '%s\n' "xfer supports Linux only" >&2
        exit 1
        ;;
esac

case "$(uname -m)" in
    x86_64 | amd64) ;;
    *)
        printf '%s\n' "xfer release binaries support x86_64 only" >&2
        exit 1
        ;;
esac

if ! command -v curl >/dev/null 2>&1; then
    printf '%s\n' "curl is required" >&2
    exit 1
fi
if ! command -v sha256sum >/dev/null 2>&1; then
    printf '%s\n' "sha256sum is required" >&2
    exit 1
fi

case "$version" in
    latest)
        download_base="https://github.com/$repo/releases/latest/download"
        ;;
    v*)
        download_base="https://github.com/$repo/releases/download/$version"
        ;;
    *)
        version="v$version"
        download_base="https://github.com/$repo/releases/download/$version"
        ;;
esac

tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT HUP INT TERM

curl --proto '=https' --tlsv1.2 --fail --location --silent --show-error \
    --output "$tmp_dir/$asset" "$download_base/$asset"
curl --proto '=https' --tlsv1.2 --fail --location --silent --show-error \
    --output "$tmp_dir/$asset.sha256" "$download_base/$asset.sha256"

(
    cd "$tmp_dir"
    sha256sum --check --status "$asset.sha256"
    tar -xzf "$asset"
)

test -x "$tmp_dir/xfer"
if ! installed_version="$("$tmp_dir/xfer" --version 2>&1)"; then
    printf '%s\n' "xfer could not start; install the Intel Level Zero runtime first" >&2
    printf '%s\n' "$installed_version" >&2
    exit 1
fi

install -d "$install_dir"
install -m 0755 "$tmp_dir/xfer" "$install_dir/xfer"

printf 'installed %s to %s/xfer\n' "$installed_version" "$install_dir"
case ":${PATH:-}:" in
    *":$install_dir:"*) ;;
    *) printf 'add %s to PATH\n' "$install_dir" ;;
esac
