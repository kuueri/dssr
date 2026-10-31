# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.0.11] - 2026-10-31

### Added

- `sudo dssr remove <id>` stops dssr, removes one source from `/etc/dssr/dssr.yaml`, deletes its records, cursor, and suspension from the index, and starts dssr again. If the source is no longer in the configuration, it deletes only the records and state in the index.
- `dssr doctor` checks the version, configuration, service, admin socket, HTTP API, index, free disk space, and the OAI-PMH endpoint of each source, and shows the fix for each failure. It also works when dssr does not start.
- `search_log` writes one JSON line per authorized `/v1/search` request, with the client name, query, filter, total, duration, and a sample of the top hits. Set `output` to `stdout` or an absolute file path; the file rotates to `search.log.01`, `search.log.02`, and so on at `max_size_mb`. The log is off until `search_log` is in `dssr.yaml`, and dssr does not delete old rotated files.

### Changed

- Releases also ship a glibc build, `dssr_linux_amd64_gnu.tar.gz`, next to the static musl build. `install.sh` uses the glibc build when it runs on the host and falls back to the static build otherwise.
- `sudo dssr uninstall` also removes the search log directory `/var/log/dssr`.

This release does not change the index, so `sudo dssr update` needs no reindex.

## [0.0.10] - 2026-10-31

### Changed

- dssr is now one static binary built in Rust, and its search index uses tantivy. The HTTP API, `dssr.yaml`, the subcommands and their options, and the systemd unit stay the same, and search returns the same totals and ranks results as 0.0.9 did.
- Search answers faster and dssr needs less memory and disk: on 13,898 records, p95 latency fell from 41 ms to 24 ms, memory from 90 MB to 38 MB, and the index from 74 MB to 29 MB.
- The cursor, generation, and suspension of each source live in `state.json` next to the index. `sudo dssr reindex` still removes them with the index.

### Removed

- The `linux/arm64` release archive. `install.sh` and `dssr update` now stop on hosts that are not `x86_64`.

This release changes the index. Run `sudo dssr update` and then `sudo dssr reindex`. Until the reindex, dssr refuses to start on the old index, so the update reports that dssr did not start.

## [0.0.9] - 2026-10-30

### Fixed

- Search no longer ignores Indonesian content words that the stop list held, such as `dini`, `diri`, `kecil`, `lama`, `lalu`, and `tidak`, so `konsep diri` no longer returns the same results as `konsep`.
- A record whose title holds the query words in order ranks higher even when the title or the query has a stop word between them, for example `putusan hakim perkara pidana` for "Putusan hakim dalam perkara pidana". Quoted phrases still match only adjacent words.
- Quoted phrases now match in creators, contributors, and subjects, not only in titles and abstracts.
- Scores no longer change when the index merges its segments, and results with equal scores come in record ID order.

This release changes the index. Run `sudo dssr update` and then `sudo dssr reindex`.

## [0.0.8] - 2026-10-30

### Added

- `dssr cancel [--source ID]` stops the harvest that is running now. The records it already read stay in the index, the cursor does not move, and a cancelled full harvest removes no records.
- `dssr suspend [--source ID]` skips the scheduled harvests and the harvest at startup until `dssr resume [--source ID]`. The suspension stays after a restart, `dssr status` shows since when, and `dssr harvest` still starts a harvest by hand. `sudo dssr reindex` removes the suspension.
- `dssr eval` shows the total result count and the expected record IDs for each query that misses.

### Changed

- A query of four or more words also finds records that match three quarters of the words, rounded down, so filler words such as `cari` or `buku` no longer hide the right record. Records that match every word still rank first, and quoted phrases must still match. For these queries `total` and the facets can grow. The existing index needs no reindex.

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

[0.0.11]: https://github.com/kuueri/dssr/compare/0.0.10...0.0.11
[0.0.10]: https://github.com/kuueri/dssr/compare/0.0.9...0.0.10
[0.0.9]: https://github.com/kuueri/dssr/compare/0.0.8...0.0.9
[0.0.8]: https://github.com/kuueri/dssr/compare/0.0.7...0.0.8
[0.0.7]: https://github.com/kuueri/dssr/compare/0.0.6...0.0.7
[0.0.6]: https://github.com/kuueri/dssr/compare/0.0.5...0.0.6
[0.0.5]: https://github.com/kuueri/dssr/compare/0.0.4...0.0.5
[0.0.4]: https://github.com/kuueri/dssr/compare/0.0.3...0.0.4
[0.0.3]: https://github.com/kuueri/dssr/compare/0.0.2...0.0.3
[0.0.2]: https://github.com/kuueri/dssr/compare/0.0.1...0.0.2
[0.0.1]: https://github.com/kuueri/dssr/releases/tag/0.0.1
