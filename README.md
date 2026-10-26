# dssr

dssr harvests metadata from repositories over OAI-PMH, keeps its own search index, and serves search over an HTTP API.
Client services search dssr, so they do not need access to the source systems.
dssr only reads the OAI-PMH endpoint of each source.

## Install

On a Linux server with systemd, run this command.
Replace the URL with the OAI-PMH base URL of your repository:

```bash
curl -fsSL https://raw.githubusercontent.com/kuueri/dssr/main/install.sh | sudo DSSR_OAI_URL=https://repository.example.org/oai/request bash
```

The script does these steps:

1. It downloads the latest release for `amd64` or `arm64` and checks the SHA-256 checksum.
2. It installs `/usr/local/bin/dssr` and the systemd service `dssr`.
3. It makes the system user `dssr`.
4. If `/etc/dssr/dssr.yaml` does not exist, it writes the file with the source `repo` from `DSSR_OAI_URL` and a new client token.
5. It starts dssr. The first harvest reads all records of each source.

At the end, the script shows the client token one time.
dssr keeps only the hash of the token, so store the token immediately.

To install a specific release, add `DSSR_VERSION=0.1.0` after `sudo`.
To upgrade, run the same command again.
The upgrade keeps `/etc/dssr/dssr.yaml`, so it does not need `DSSR_OAI_URL`.

dssr listens on `127.0.0.1:8080` and does not use TLS.
Put a TLS reverse proxy in front of dssr before other hosts use the API.

## Configure

Edit `/etc/dssr/dssr.yaml`, then restart dssr with `sudo systemctl restart dssr`.
dssr does not start if the file has an unknown key or an incorrect value.

| Key            | Value                                                                                                    |
| -------------- | -------------------------------------------------------------------------------------------------------- |
| `listen`       | TCP address of the HTTP API                                                                              |
| `index_path`   | Directory of the index                                                                                   |
| `admin_socket` | Path of the unix socket for the `dssr` subcommands                                                       |
| `timezone`     | Time zone for the schedules                                                                              |
| `clients`      | `name` and `token_sha256` of each client service                                                         |
| `sources`      | `id`, `name`, `oai_url`, `metadata_prefix` (only `dim`), and `schedule` (cron expression) of each source |

To add a client service, make a token and its hash:

```bash
TOKEN=$(openssl rand -hex 32)
printf '%s' "$TOKEN" | sha256sum
```

Add the hash to `clients`, restart dssr, and give the token to the client service.

## Use

On the server, run the `dssr` subcommands as the `dssr` user:

```bash
sudo -u dssr dssr status --config /etc/dssr/dssr.yaml
sudo -u dssr dssr search "wisata ziarah" --year 2020-2024 --config /etc/dssr/dssr.yaml
sudo -u dssr dssr get repo:123456789/56 --config /etc/dssr/dssr.yaml
sudo -u dssr dssr harvest --full --config /etc/dssr/dssr.yaml
```

| Command                               | Action                                                                     |
| ------------------------------------- | -------------------------------------------------------------------------- |
| `dssr status`                         | Show the record count, cursor, last run, and dropped fields of each source |
| `dssr search <query>`                 | Search with the same logic as the HTTP API                                 |
| `dssr get <id>`                       | Show one record                                                            |
| `dssr harvest [--source ID] [--full]` | Start a harvest now                                                        |
| `dssr eval <file>`                    | Measure search relevance with a list of queries                            |

Client services use the HTTP API with a bearer token:

```bash
curl -H "Authorization: Bearer $TOKEN" "http://127.0.0.1:8080/v1/search?q=wisata+ziarah&limit=5"
```

| Endpoint                            | Purpose                                     |
| ----------------------------------- | ------------------------------------------- |
| `GET /v1/search`                    | Search with filters, highlights, and facets |
| `GET /v1/records/{source}/{handle}` | Get one record                              |
| `GET /openapi.json`                 | Full API description, without a token       |
| `GET /healthz`                      | Liveness check, without a token             |

## Important behavior

- Data in dssr is only as fresh as the OAI-PMH data of the source. Set `schedule` to run after the source refreshes its OAI-PMH data.
- An incremental harvest cannot find items that DSpace expunged. Run `dssr harvest --full` to delete them from the index.
- A full harvest does not delete old records if it sees fewer than 90% of the indexed records. This guard keeps the index if the source fails.
- `dssr status` shows the `dim` fields that dssr dropped. Use this list to extend the field mapping in `internal/record`.
- The index keeps the analyzer and the index mapping from the time dssr made it. After a change to `internal/index`, stop dssr, delete the index directory, and start dssr.

## Build from source

With Go 1.27.3 or later:

```bash
CGO_ENABLED=0 go build -trimpath -o dssr ./cmd/dssr
```

## License

dssr is licensed under the [Apache License 2.0](LICENSE).
