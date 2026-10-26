# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.0.2] - 2026-10-27

### Added

- `dssr uninstall` stops and disables the service, then removes the installed binary, systemd unit, configuration, index, runtime directory, `dssr` user and group, and mailbox. The index is deleted permanently.
- `dssr update` installs the latest release when it is newer than the installed version, runs `install.sh` from the release tag, keeps `/etc/dssr/dssr.yaml`, and restarts the service.

## [0.0.1] - 2026-10-26

### Added

- OAI-PMH harvester for the `dim` metadata format, with a cron schedule for each source, an incremental harvest at startup, and `dssr harvest --full` to delete records that a source no longer lists.
- Search index with BM25 scoring, Indonesian and English stop words, fuzzy matching, quoted phrases, filters, facets, and highlights.
- HTTP API with bearer tokens: `GET /v1/search`, `GET /v1/records/{source}/{handle}`, `GET /openapi.json`, and `GET /healthz`.
- Command-line interface: `serve`, `harvest`, `search`, `get`, `status`, and `eval`.
- `install.sh` to install or upgrade dssr as a systemd service from a GitHub release.
- Release archives for linux/amd64 and linux/arm64 with SHA-256 checksums and the licenses of all dependencies.

[0.0.2]: https://github.com/kuueri/dssr/compare/0.0.1...0.0.2
[0.0.1]: https://github.com/kuueri/dssr/releases/tag/0.0.1
