#!/bin/sh
set -eu

version="1.32.0"
base_url="https://github.com/oneapi-src/level-zero/releases/download/v$version"
runtime_package="libze1_${version}+u22.04_amd64.deb"
development_package="libze-dev_${version}+u22.04_amd64.deb"
runtime_sha256="ce124a8f9cd049bc3177d940b4aacacbc4c5087ca7c0a844639b9c9c9c23a9ec"
development_sha256="b1410fc501b1b453fd544569ee76221e9946ea89cadd4061ffaf95e0e452ba63"

tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT HUP INT TERM

curl --proto '=https' --tlsv1.2 --fail --location --silent --show-error \
    --output "$tmp_dir/$runtime_package" "$base_url/$runtime_package"
curl --proto '=https' --tlsv1.2 --fail --location --silent --show-error \
    --output "$tmp_dir/$development_package" "$base_url/$development_package"

printf '%s  %s\n' "$runtime_sha256" "$tmp_dir/$runtime_package" | sha256sum --check -
printf '%s  %s\n' "$development_sha256" "$tmp_dir/$development_package" | sha256sum --check -

sudo apt-get update
sudo apt-get install --yes \
    libclang-dev \
    pkg-config \
    "$tmp_dir/$runtime_package" \
    "$tmp_dir/$development_package"
