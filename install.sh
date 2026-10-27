#!/usr/bin/env bash
# Install or upgrade dssr as a systemd service from a GitHub release.
#
#   curl -fsSL https://raw.githubusercontent.com/kuueri/dssr/main/install.sh | sudo DSSR_OAI_URL=https://example.org/oai/request bash
#
# Environment:
#   DSSR_OAI_URL  OAI-PMH base URL of the first source, required when /etc/dssr/dssr.yaml does not exist
#   DSSR_VERSION  release tag to install, for example 0.1.0 (default: the latest release)
#
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

write_config() {
	local hash=$1 url=$2 tmp=${WORK_DIR}/dssr.yaml host
	host=${url#*://}
	host=${host%%[:/]*}
	tee "${tmp}" >/dev/null <<EOF
listen: 127.0.0.1:8080
index_path: /var/lib/dssr/index
admin_socket: /run/dssr/admin.sock
timezone: Asia/Jakarta

clients:
    - name: default
      token_sha256: ${hash}

sources:
    - id: repo
      name: "${host}"
      oai_url: "${url}"
      metadata_prefix: dim
      schedule: "0 3 * * 1"
EOF
	install -m 0640 -o root -g dssr "${tmp}" "${CONF}"
}

main() {
	[[ ${EUID} -eq 0 ]] || die "run as root: curl -fsSL https://raw.githubusercontent.com/${REPO}/main/install.sh | sudo bash"
	local url=${DSSR_OAI_URL:-}
	if [[ ! -e ${CONF} ]]; then
		[[ -n ${url} ]] || die "the first install needs DSSR_OAI_URL: curl -fsSL https://raw.githubusercontent.com/${REPO}/main/install.sh | sudo DSSR_OAI_URL=https://example.org/oai/request bash"
		[[ ${url} =~ ^https?://[A-Za-z0-9.-]+(:[0-9]+)?(/[A-Za-z0-9._~/-]*)?$ ]] || die "DSSR_OAI_URL must be an http or https URL without a query"
	fi
	local cmd
	for cmd in curl awk tar sha256sum od install useradd usermod systemctl journalctl; do
		command -v "${cmd}" >/dev/null || die "${cmd} is required"
	done

	local arch
	case $(uname -m) in
		x86_64) arch=amd64 ;;
		aarch64 | arm64) arch=arm64 ;;
		*) die "unsupported architecture $(uname -m)" ;;
	esac

	local base=https://github.com/${REPO}/releases/latest/download
	if [[ -n ${DSSR_VERSION:-} ]]; then
		[[ ${DSSR_VERSION} =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "DSSR_VERSION must be a tag like 0.1.0"
		base=https://github.com/${REPO}/releases/download/${DSSR_VERSION}
	fi

	trap cleanup EXIT
	WORK_DIR=$(mktemp -d)

	local file archive=dssr_linux_${arch}.tar.gz
	for file in "${archive}" checksums.txt; do
		log "downloading ${base}/${file}"
		"${CURL[@]}" -o "${WORK_DIR}/${file}" "${base}/${file}" || die "cannot download ${base}/${file}"
	done
	(cd "${WORK_DIR}" && awk -v f="${archive}" '$2 == f' checksums.txt | sha256sum -c --quiet -) ||
		die "checksum mismatch for ${archive}"

	local pkg=${WORK_DIR}/pkg
	mkdir -p "${pkg}"
	tar -xzf "${WORK_DIR}/${archive}" -C "${pkg}"

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
		token=$(head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n')
		write_config "$(printf '%s' "${token}" | sha256sum | cut -d ' ' -f 1)" "${url}"
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
	printf '  Configuration  %s\n' "${CONF}"
	printf '  Status         dssr status\n'
	printf '  Log            journalctl -u dssr -f\n'
	printf '  HTTP API       http://127.0.0.1:8080\n'
	if [[ -n ${token} ]]; then
		printf '\n  Client token   %s\n' "${token}"
		printf '  dssr keeps only the hash of this token. Store the token now.\n'
	fi
	if [[ -n ${added} ]]; then
		printf '\n  %s is now in the group dssr. Log in again to run dssr without sudo.\n' "${added}"
	fi
}

main "$@"
