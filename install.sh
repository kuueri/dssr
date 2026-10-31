#!/usr/bin/env bash
# Install or upgrade dssr as a systemd service from a GitHub release.
#
#   curl -fsSL https://kuueri.com/dssr/install.sh | sudo bash
#
# Environment:
#   DSSR_VERSION  release tag to install, for example 0.1.0 (default: the latest release)
#
# On a host with glibc, the script installs the glibc build if it runs there; otherwise it installs the static musl build.
# The first run writes /etc/dssr/dssr.yaml with a client token and no source; add the sources there and restart dssr.
# A second run upgrades the binary and the unit file, keeps /etc/dssr/dssr.yaml, and restarts dssr.
# The user that runs sudo joins the group dssr, so it can run the dssr subcommands without sudo.
set -Eeuo pipefail
shopt -s inherit_errexit
IFS=$'\n\t'

readonly REPO=kuueri/dssr
readonly BIN=/usr/local/bin/dssr
readonly CONF_DIR=/etc/dssr
readonly CONF=${CONF_DIR}/dssr.yaml
readonly UNIT=/etc/systemd/system/dssr.service
readonly DOC_DIR=/usr/local/share/doc/dssr
readonly CURL=(curl -fsSL --connect-timeout 15 --retry 3)

WORK_DIR=""
PKG=""

log() { printf 'dssr-install: %s\n' "$*" >&2; }
die() {
	log "error: $*"
	exit 1
}

cleanup() {
	local code=$?
	if [[ -n ${WORK_DIR} && -d ${WORK_DIR} ]]; then
		rm -rf -- "${WORK_DIR}"
	fi
	exit "${code}"
}

# Download one archive, check it against checksums.txt, and unpack it into PKG.
# A failed download returns 1, so the caller can fall back; a checksum mismatch stops the script.
fetch() {
	local base=$1 archive=$2
	local dir=${WORK_DIR}/${archive%.tar.gz}
	log "downloading ${base}/${archive}"
	"${CURL[@]}" -o "${WORK_DIR}/${archive}" "${base}/${archive}" || return 1
	(cd "${WORK_DIR}" && awk -v f="${archive}" '$2 == f' checksums.txt | sha256sum -c --quiet -) ||
		die "checksum mismatch for ${archive}"
	mkdir -p "${dir}" || return 1
	tar -xzf "${WORK_DIR}/${archive}" -C "${dir}" || return 1
	PKG=${dir}
}

write_config() {
	local hash=$1 tmp=${WORK_DIR}/dssr.yaml
	tee "${tmp}" >/dev/null <<EOF
listen: 127.0.0.1:8080
index_path: /var/lib/dssr/index
admin_socket: /run/dssr/admin.sock
timezone: Asia/Jakarta

# Log each API search as one JSON line, for dssr eval.
# search_log:
#   output: /var/log/dssr/search.log   # or stdout
#   hits: 10
#   max_size_mb: 64

clients:
  - name: default
    token_sha256: ${hash}

# Add each OAI-PMH source, then run: sudo systemctl restart dssr
# sources:
#   - id: repo
#     name: Institutional Repository
#     oai_url: https://example.org/oai/request
#     metadata_prefix: oai_dc
#     schedule: "0 3 * * 1"
EOF
	install -m 0640 -o root -g dssr "${tmp}" "${CONF}"
}

main() {
	[[ ${EUID} -eq 0 ]] || die "run as root: curl -fsSL https://kuueri.com/dssr/install.sh | sudo bash"
	local cmd
	for cmd in curl awk tar sha256sum od install useradd usermod systemctl journalctl; do
		command -v "${cmd}" >/dev/null || die "${cmd} is required"
	done

	local arch
	case $(uname -m) in
		x86_64) arch=amd64 ;;
		*) die "unsupported architecture $(uname -m)" ;;
	esac

	local base=https://github.com/${REPO}/releases/latest/download
	if [[ -n ${DSSR_VERSION:-} ]]; then
		[[ ${DSSR_VERSION} =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "DSSR_VERSION must be a tag like 0.1.0"
		base=https://github.com/${REPO}/releases/download/${DSSR_VERSION}
	fi

	trap cleanup EXIT
	WORK_DIR=$(mktemp -d)

	log "downloading ${base}/checksums.txt"
	"${CURL[@]}" -o "${WORK_DIR}/checksums.txt" "${base}/checksums.txt" || die "cannot download ${base}/checksums.txt"

	local build=musl
	if getconf GNU_LIBC_VERSION >/dev/null 2>&1; then
		# shellcheck disable=SC2310 # fetch returns 1 on each failed step, so the caller can fall back
		if fetch "${base}" "dssr_linux_${arch}_gnu.tar.gz" && "${PKG}/dssr" --version >/dev/null 2>&1; then
			build=glibc
		else
			log "the glibc build does not run on this host; using the static musl build"
		fi
	fi
	if [[ ${build} == musl ]]; then
		# shellcheck disable=SC2310 # fetch returns 1 on each failed step
		fetch "${base}" "dssr_linux_${arch}.tar.gz" || die "cannot download ${base}/dssr_linux_${arch}.tar.gz"
	fi
	local pkg=${PKG}

	install -m 0755 "${pkg}/dssr" "${BIN}"
	install -d -m 0755 "${DOC_DIR}"
	rm -rf -- "${DOC_DIR:?}/third_party"
	cp -r -- "${pkg}/LICENSE" "${pkg}/third_party" "${DOC_DIR}/"

	if ! id -u dssr >/dev/null 2>&1; then
		log "creating system user dssr"
		useradd --system --no-create-home --shell "$(command -v nologin || echo /bin/false)" dssr
	fi

	local added=""
	if [[ -n ${SUDO_USER:-} && ${SUDO_USER} != root && " $(id -nG "${SUDO_USER}") " != *" dssr "* ]]; then
		log "adding ${SUDO_USER} to group dssr"
		if usermod -aG dssr "${SUDO_USER}"; then
			added=${SUDO_USER}
		else
			log "warning: cannot add ${SUDO_USER} to group dssr"
		fi
	fi

	local token=""
	if [[ ! -e ${CONF} ]]; then
		log "writing ${CONF}"
		install -d -m 0755 "${CONF_DIR}"
		token=sk-$(head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n')
		write_config "$(printf '%s' "${token}" | sha256sum | cut -d ' ' -f 1)"
		printf '\n  Client token   %s\n' "${token}"
		printf '  dssr keeps only the hash of this token. Store the token now.\n\n'
	else
		log "keeping ${CONF}"
	fi

	install -m 0644 "${pkg}/dssr.service" "${UNIT}"
	systemctl daemon-reload
	systemctl enable --quiet dssr
	systemctl restart dssr
	sleep 2
	if ! systemctl is-active --quiet dssr; then
		journalctl -u dssr -n 20 --no-pager >&2 || true
		die "dssr did not start. Correct ${CONF}, then run: systemctl restart dssr"
	fi

	printf '\ndssr %s is installed and running.\n\n' "$("${BIN}" --version)"
	printf '  Build          %s\n' "${build}"
	printf '  Configuration  %s\n' "${CONF}"
	printf '  Status         dssr status\n'
	printf '  Log            journalctl -u dssr -f\n'
	printf '  HTTP API       http://%s\n' "$(awk '$1 == "listen:" { print $2; exit }' "${CONF}" | tr -d "\"'")"
	if [[ -n ${token} ]]; then
		printf '\n  dssr has no source yet. Add the sources to %s, then run: sudo systemctl restart dssr\n' "${CONF}"
	fi
	if [[ -n ${added} ]]; then
		printf '\n  %s is now in the group dssr. Log in again to run dssr without sudo.\n' "${added}"
	fi
}

main "$@"
