# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.0.7] - 2026-10-27

### Added

- `sudo dssr revoke` replaces the client token for `default`, writes the hash of the new token to the configuration, restarts dssr, and shows the token one time. The previous token stops working.

### Changed

- New tokens from `install.sh` and `dssr revoke` start with `sk-`; existing tokens keep working until they are revoked.

### Fixed

- The incremental harvest sends `from` as a date without a time, because SLiMS rejects a timestamp although its `Identify` declares seconds. OAI-PMH requires every repository to accept a date.

## [0.0.6] - 2026-10-27

### Fixed

- `install.sh` prints the client token as soon as it writes the configuration, and the install summary reports the `listen` address from that file instead of the fixed `127.0.0.1:8080`.

## [0.0.5] - 2026-10-27

### Added

- `sudo dssr reindex` stops dssr, deletes the index, and starts dssr, which then harvests every record of each source again. Run it after a release that changes the index, such as this one.
- `dssr <command> --help` describes each command and its options.

### Changed

- Search matches Indonesian words that differ only in affixes, for example `pembelajaran` and `belajar`, or `perkantoran` and `kantor`. An existing index keeps searching without this until you run `sudo dssr update` and then `sudo dssr reindex`.
- `install.sh` no longer needs `DSSR_OAI_URL`. A new install writes `/etc/dssr/dssr.yaml` with a client token and an example source in comments; add the sources there and run `sudo systemctl restart dssr`. dssr now starts without a source and harvests nothing until the file has one.
- The install command is now `curl -fsSL https://kuueri.com/dssr/install.sh | sudo bash`, which runs `install.sh` from the latest release. Each release carries `install.sh` as an asset.
- dssr is marked experimental: the API, the configuration, and the index format may change without notice.

### Fixed

- `dssr update` with `DSSR_VERSION` does nothing when that release is not newer than the installed version, instead of installing it.

## [0.0.4] - 2026-10-27

### Added

- Harvest the `oai_dc` metadata format alongside `dim`. `oai_dc` is the default for a source that omits `metadata_prefix`, and `dim` remains available for the DSpace-specific mapping.
- `dssr update` accepts `DSSR_VERSION`, so it can install a specific release, including a pre-release, instead of the latest release.

### Changed

- New installs and new sources use `metadata_prefix: oai_dc`; existing configs keep their prefix. Run `dssr harvest --full --source ID` after changing the prefix on an existing source.

## [0.0.3] - 2026-10-27

### Added

- Members of the group `dssr` can run the `dssr` subcommands without `sudo`, because the admin socket is group-readable and `install.sh` adds the user that runs `sudo` to the group.

### Changed

- The `dssr` subcommands read `/etc/dssr/dssr.yaml` by default, so they run from any directory without `--config`.

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

[0.0.7]: https://github.com/kuueri/dssr/compare/0.0.6...0.0.7
[0.0.6]: https://github.com/kuueri/dssr/compare/0.0.5...0.0.6
[0.0.5]: https://github.com/kuueri/dssr/compare/0.0.4...0.0.5
[0.0.4]: https://github.com/kuueri/dssr/compare/0.0.3...0.0.4
[0.0.3]: https://github.com/kuueri/dssr/compare/0.0.2...0.0.3
[0.0.2]: https://github.com/kuueri/dssr/compare/0.0.1...0.0.2
[0.0.1]: https://github.com/kuueri/dssr/releases/tag/0.0.1
