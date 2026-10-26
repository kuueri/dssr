# Project

dssr is a discovery backend for any OAI-PMH data provider, built first for a university library.
It harvests metadata over OAI-PMH, keeps its own Bleve search index, and serves search over an HTTP API, so client services never touch the source systems.

## Relationship to VuFind

The problem statement comes from VuFind (<https://github.com/vufind-org/vufind>): pull records into a local search index, then answer discovery queries without querying the source per request.

The approach differs, and it is platform-agnostic. VuFind is a PHP portal with a UI, Solr, ILS drivers, and a separate OAI-PMH harvester; dssr is a headless Go backend with an embedded Bleve index and an HTTP API as its only interface. Any source that exposes OAI-PMH with the mapped metadata prefix can be harvested; there is no UI and no ILS driver layer.

Interoperability rests on published contracts: OAI-PMH 2.0 for harvesting, the `dim` prefix as the only mapped payload (whitelisted to `dc` fields), and OpenAPI 3.1 for the HTTP API. dssr is not a VuFind replacement.

## Workflow guardrails

Never run `go build`, `go test`, `go fmt`, `git add`, or `git commit`.
End each code change with the exact command for the user to run instead:

```bash
go test -race ./...
CGO_ENABLED=0 go build -trimpath -o dssr ./cmd/dssr
gofmt -l .
shellcheck install.sh scripts/package.sh
```

Run read-only commands in their non-interactive form, because a pager blocks the session: `git --no-pager diff`, `git --no-pager log`, `journalctl --no-pager`, `systemctl --no-pager status`.

## Scope

dssr is pull-only: it reaches each source only through the source's OAI-PMH endpoint, and every change stays inside this repository.

## Architecture

`dssr serve` is the single owner of the Bleve index, because bbolt holds an exclusive file lock.
Every subcommand except `update` and `uninstall` is an HTTP client of the admin unix socket (`call` in `cmd/dssr/main.go`), so it needs a running `serve`.
`dssr update` runs `install.sh` from the tag of the latest release with `DSSR_VERSION` set, never from `main`, so the installer matches the release files.
`dssr uninstall` stops the service and removes the installation from the host.
Both run as root and do not contact `serve`.

A harvest flows through `harvest` (schedule), `oai` (`ListRecords` paging), `record` (`dim` whitelist mapping), and `index` (Bleve documents).
Each document stores the full record as JSON in the unindexed field `record`, and search results and record lookups return that JSON.

## Invariants

- The incremental `from` is the cursor minus 24 hours, because a DSpace datestamp is the change time, not the time the record reached the OAI core. The cursor advances only after a successful run.
- A full harvest is mark-and-sweep by generation. The sweep is skipped when the run fails or sees fewer than 90% of the indexed records, because DSpace answers `noRecordsMatch` when its OAI core is empty or Solr fails.
- Bleve persists its index mapping at creation, so a change to `newMapping` or the analyzer requires deleting the index directory, while a change to the `record` mapping requires `dssr harvest --full`.
- The whitelist keeps only the `dc` schema and drops `dc.description.provenance` (staff names and e-mail addresses) and every non-`dc` schema (may hold student numbers). `dssr status` counts dropped fields, and that count is how the whitelist grows.
- Results carry the full record because clients pass them to AI agents as context.
- Search weights, fuzzy settings, and stemming change only on evidence from `dssr eval`.

## Adding a mapped field

A field is mapped when these five places agree:

1. the `Record` struct in `internal/record/record.go`,
2. the `add` switch in the same file,
3. the fixture `internal/record/testdata/dim.xml`,
4. the expected record in `internal/record/record_test.go`,
5. the `Record` schema in `internal/server/openapi.json`.

## Release

Tags are bare SemVer (`0.2.0`), and only the GitHub release title carries the `v` (`v0.2.0`).
Go's VCS stamping ignores tags without `v`, so `scripts/package.sh` sets the version through `-ldflags "-X main.buildVersion=<tag>"`.
Asset names carry no version because `install.sh` downloads from `releases/latest/download/<name>`.

`CHANGELOG.md` changes only in a release commit.
When the user asks for release `X.Y.Z`:

1. Read every commit since the previous tag with `git --no-pager log --oneline <previous-tag>..HEAD`, or the whole history when no tag exists.
2. Add `## [X.Y.Z] - YYYY-MM-DD` above the previous version section, with entries under the Keep a Changelog headings (Added, Changed, Deprecated, Removed, Fixed, Security) written for operators, not per commit. The step is done when every user-visible change since the previous tag has an entry.
3. Add the link reference `[X.Y.Z]: https://github.com/kuueri/dssr/compare/<previous-tag>...X.Y.Z`, or `https://github.com/kuueri/dssr/releases/tag/X.Y.Z` for the first release.
4. Hand the user these commands: `git add CHANGELOG.md`, `git commit -m "release: X.Y.Z"`, `git tag -s X.Y.Z -m X.Y.Z`, `git push origin main X.Y.Z`.

The release workflow publishes the new section, followed by GitHub's generated change list, as the release notes.
It fails when the tag is not a signed annotated tag that GitHub verifies, or when the section for the tag is missing or has no `- ` entries.
