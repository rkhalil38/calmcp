# calmcp

A stateless [MCP](https://modelcontextprotocol.io) server for CalDAV. It's configured entirely through environment variables and serves MCP over streamable HTTP at `/mcp`.

## Tools

| Tool | Description |
|---|---|
| `list_calendars` | List the user's calendars. |
| `list_events` | List events in a time range. Recurring events are expanded by the CalDAV server. |
| `create_event` | Create an event. Returns its id and etag. |
| `update_event` | Update an event, a whole recurring series, or a single occurrence. Only the given fields change. |
| `delete_event` | Delete an event, a whole recurring series, or a single occurrence. |
| `find_free_time` | Find free slots across calendars, optionally limited to daily hours (e.g. 09:00–17:00). |

Times can be given as `YYYY-MM-DD` (all-day), `YYYY-MM-DDTHH:MM` (local time in `TZ`), or RFC 3339.

## Configuration

| Variable | Required | Default | Description |
|---|---|---|---|
| `CALDAV_URL` | yes | | CalDAV server URL (`http` or `https`) |
| `CALDAV_USERNAME` | yes | | CalDAV username |
| `CALDAV_PASSWORD` | yes | | CalDAV password |
| `MCP_API_KEY` | yes | | Clients must send `Authorization: Bearer <key>` |
| `MCP_ALLOWED_HOSTS` | no | `localhost,127.0.0.1,::1` | Comma-separated hostnames (optionally `host:port`) clients may use in the `Host` header. Set this to the public hostname when not accessed via localhost, or `*` to allow any |
| `PORT` | no | `8000` | Port to listen on |
| `TZ` | no | `Etc/UTC` | IANA timezone used to interpret local times |
| `RUST_LOG` | no | | Log level, e.g. `info` |

## Running

### From source

```sh
CALDAV_URL=https://dav.example.com/ \
CALDAV_USERNAME=me \
CALDAV_PASSWORD=... \
MCP_API_KEY=... \
cargo run --release
```

### Docker

Images are published to `ghcr.io/rkhalil38/calmcp` and signed with cosign:

| Trigger | Tag |
|---|---|
| Push to `master` | `master` |
| Git tag `v1.2.3` | `v1.2.3`, `latest` |
| Nightly rebuild | `nightly` |

The container listens on port 8000 unless `PORT` is set.

```sh
docker run -p 8000:8000 \
  -e CALDAV_URL=https://dav.example.com/ \
  -e CALDAV_USERNAME=me \
  -e CALDAV_PASSWORD=... \
  -e MCP_API_KEY=... \
  -e TZ=America/New_York \
  ghcr.io/rkhalil38/calmcp:master
```

To build locally: `docker build -t calmcp .`

### Connecting a client

Connect a client, e.g. Claude Code:

```sh
claude mcp add --transport http calmcp http://localhost:8000/mcp \
  --header "Authorization: Bearer <MCP_API_KEY>"
```

## Development

A local Radicale server (user `test` / `test`) is provided in `dev/radicale`:

```sh
docker compose -f dev/radicale/docker-compose.yml up -d
dev/radicale/seed.sh      # creates "Personal" and "Work" calendars with a sample event
scripts/run-mcp.sh        # runs calmcp against it on :8000 with MCP_API_KEY=dev
```

## Notes

- Recurring events are expanded server-side via CalDAV `expand` (RFC 4791 §9.6.5), so calmcp has no RRULE logic of its own. Only tested against Radicale 3.5.4.
- Updates and deletes take an `etag`, so changes made elsewhere in the meantime are not overwritten.
