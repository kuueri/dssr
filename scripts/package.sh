#!/usr/bin/env bash
# Build the release archives, checksums.txt, and notes.md in dist/ for one tag.
#
#   bash scripts/package.sh 0.1.0
#
# The tag is a bare SemVer version without "v", and CHANGELOG.md must have a section for it.
# It builds two variants: dssr_linux_amd64 is a static musl binary for every Linux host, and dssr_linux_amd64_gnu links the glibc of the build host.
# The musl build needs the rustup target x86_64-unknown-linux-musl and musl-gcc, which ring uses to compile its C code.
set -Eeuo pipefail
shopt -s inherit_errexit nocasematch
IFS=$'\n\t'

readonly MUSL=x86_64-unknown-linux-musl
readonly GNU=x86_64-unknown-linux-gnu

die() {
	printf 'package: %s\n' "$*" >&2
	exit 1
}

# Print name, directory, license, and repository of every crate linked into the binaries.
crates() {
	local linked
	linked=$(cargo tree --locked -e normal,build --target "${MUSL}" --prefix none --format '{p}' |
		awk 'NF && $1 != "dssr" { print $1 " " substr($2, 2) }' | sort -u)
	cargo metadata --format-version 1 --locked |
		jq -r --arg linked "${linked}" '
			($linked | split("\n") | map({(.): true}) | add) as $want
			| .packages[]
			| select($want[.name + " " + .version])
			| [.name, (.manifest_path | rtrimstr("/Cargo.toml")), (.license // "unknown"), (.repository // "")]
			| @tsv'
}

license_files() {
	local f
	for f in "$1"/*; do
		[[ -f ${f} ]] || continue
		case ${f##*/} in
			license* | copying* | notice* | unlicense*) printf '%s\n' "${f}" ;;
			*) ;;
		esac
	done
}

# A crate without a license file takes the files of a crate from the same repository, or else a note with its SPDX expression.
collect_licenses() {
	local out=$1 list name dir license repo files f
	local -A by_repo=()
	list=$(crates)
	[[ -n ${list} ]] || die "cargo tree listed no crates"
	while IFS=$'\t' read -r name dir license repo; do
		files=$(license_files "${dir}")
		if [[ -n ${files} && -n ${repo} ]]; then
			by_repo[${repo}]=${files}
		fi
	done <<<"${list}"
	while IFS=$'\t' read -r name dir license repo; do
		mkdir -p "${out}/${name}"
		files=$(license_files "${dir}")
		if [[ -z ${files} && -n ${repo} ]]; then
			files=${by_repo[${repo}]:-}
		fi
		if [[ -n ${files} ]]; then
			while IFS= read -r f; do
				cp -- "${f}" "${out}/${name}/"
			done <<<"${files}"
		else
			printf 'SPDX-License-Identifier: %s\nSource: %s\n' "${license}" "${repo}" >"${out}/${name}/LICENSE"
		fi
	done <<<"${list}"
}

# Build one target and pack it with the license files as dist/<name>.tar.gz.
package() {
	local tag=$1 target=$2 name=$3
	local dir=dist/${name}
	mkdir -p "${dir}"
	DSSR_BUILD_VERSION=${tag} CC_x86_64_unknown_linux_musl=musl-gcc cargo build --locked --release --target "${target}"
	cp -- "target/${target}/release/dssr" LICENSE deploy/dssr.service "${dir}/"
	cp -r -- dist/third_party "${dir}/"
	tar -C "${dir}" -czf "dist/${name}.tar.gz" .
}

release_notes() {
	awk -v heading="## [$1]" '
		index($0, heading) == 1 { found = 1; next }
		found && (/^## \[/ || /^\[[^]]+\]: /) { exit }
		found { print }
	' CHANGELOG.md
}

main() {
	local tag=${1:?usage: scripts/package.sh <tag>}
	[[ ${tag} =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "the tag must be a bare version like 0.1.0, got ${tag}"
	cd "$(dirname "$0")/.."
	local cmd
	for cmd in cargo jq musl-gcc; do
		command -v "${cmd}" >/dev/null || die "${cmd} is required"
	done

	local notes
	notes=$(release_notes "${tag}")
	[[ $'\n'${notes} == *$'\n- '* ]] || die "CHANGELOG.md has no section with entries for ${tag}"

	rm -rf -- dist
	mkdir -p dist/third_party
	printf '%s\n' "${notes}" >dist/notes.md
	collect_licenses dist/third_party

	package "${tag}" "${MUSL}" dssr_linux_amd64
	package "${tag}" "${GNU}" dssr_linux_amd64_gnu

	(cd dist && sha256sum dssr_linux_amd64.tar.gz dssr_linux_amd64_gnu.tar.gz >checksums.txt)
	printf 'package: built %s\n' "${tag}"
}

if [[ ${BASH_SOURCE[0]} == "$0" ]]; then
	main "$@"
fi
