#!/usr/bin/env bash
# Build the release archives, checksums.txt, and notes.md in dist/ for one tag.
#
#   bash scripts/package.sh 0.1.0
#
# The tag is a bare SemVer version without "v", and CHANGELOG.md must have a section for it.
set -Eeuo pipefail
shopt -s inherit_errexit nocasematch
IFS=$'\n\t'

readonly ARCHES=(amd64 arm64)

die() {
	printf 'package: %s\n' "$*" >&2
	exit 1
}

collect_licenses() {
	local out=$1 mods path dir f found
	mods=$(go list -deps -f '{{with .Module}}{{if not .Main}}{{.Path}} {{.Dir}}{{end}}{{end}}' ./cmd/dssr | sort -u)
	while IFS=' ' read -r path dir; do
		found=false
		mkdir -p "${out}/${path}"
		for f in "${dir}"/*; do
			[[ -f ${f} ]] || continue
			case ${f##*/} in
				license* | copying* | notice*)
					cp -- "${f}" "${out}/${path}/"
					found=true
					;;
				*) ;;
			esac
		done
		[[ ${found} == true ]] || die "no license file in ${path}"
	done <<<"${mods}"
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

	local notes
	notes=$(release_notes "${tag}")
	[[ $'\n'${notes} == *$'\n- '* ]] || die "CHANGELOG.md has no section with entries for ${tag}"

	rm -rf -- dist
	mkdir -p dist/third_party
	printf '%s\n' "${notes}" >dist/notes.md
	collect_licenses dist/third_party

	local arch dir
	for arch in "${ARCHES[@]}"; do
		dir=dist/dssr_linux_${arch}
		mkdir -p "${dir}"
		GOOS=linux GOARCH=${arch} CGO_ENABLED=0 go build -trimpath -ldflags "-X main.buildVersion=${tag}" -o "${dir}/dssr" ./cmd/dssr
		cp -- LICENSE deploy/dssr.service "${dir}/"
		cp -r -- dist/third_party "${dir}/"
		tar -C "${dir}" -czf "dist/dssr_linux_${arch}.tar.gz" .
	done

	(cd dist && sha256sum dssr_linux_*.tar.gz >checksums.txt)
	printf 'package: built %s\n' "${tag}"
}

main "$@"
