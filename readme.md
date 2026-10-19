# seerr-mcp

An [MCP](https://modelcontextprotocol.io) server that lets each person in a household search for films
and series in [Seerr](https://github.com/seerr-team/seerr) (Jellyseerr / Overseerr), request one as
themselves after confirming it in chat, and hear back when it is ready on Jellyfin.

Each person has their own bearer token. The server maps that token to their Seerr user, so a request
is filed in their name and counts against their quota. When Seerr reports that a title is available,
the person who asked is told in their own chat room through a
[Hermes Agent](https://github.com/NousResearch/hermes-agent) webhook.

## Tools

| Tool | What it does |
|------|--------------|
| `search_media` | The top three films and series matching a query, with what Seerr already knows about each: available, requested (by whom, when) or not yet, and a series' season numbers. Each hit shows its title in the person's language, in the other supported language, and its original title. A query ending in a year that finds nothing is retried once without the year. Read-only. |
| `request_media` | Files a request as the caller, but only after the chat client confirms it ([MCP form elicitation](https://modelcontextprotocol.io/specification/draft/client/elicitation)). A client that cannot ask files nothing. |
| `my_requests` | The caller's ten latest requests and where each one is. Read-only. |

The caller is whoever holds the bearer token. Their Seerr user comes from the identities file, never
from a tool argument.

## Routes

All on `SEERR_MCP_ADDR` (default `0.0.0.0:8080`).

| Route | Auth | Purpose |
|-------|------|---------|
| `/mcp` | `Authorization: Bearer <token>` | Streamable HTTP MCP |
| `/notify` | `X-Seerr-Mcp-Secret` | Seerr's webhook. A `MEDIA_AVAILABLE` for someone with a chat room becomes one signed POST to `{HERMES_WEBHOOK_BASE}/p/<profile>/webhooks/ready-<profile>`, so Hermes needs one `ready-<profile>` route per person. Answers 401 (bad secret), 400 (bad payload), 204 (nothing to send), 200 (delivered) or 502 (Hermes refused or was unreachable). |
| `/metrics` | none | Prometheus |
| `/healthz` | none | Liveness, for a human with curl. The image is distroless, so it has no healthcheck. |

`/metrics` and `/healthz` are unauthenticated, so do not publish the port to the internet. Put `/mcp`
behind TLS if the chat agent reaches it across a network.

## Configuration

Every variable is required unless marked optional, and an empty value refuses to start.

| Variable | |
|----------|--|
| `SEERR_URL` | Seerr's base URL, e.g. `http://seerr:5055` |
| `SEERR_API_KEY` | Seerr's admin API key (Settings → General). Each call names its user with `X-API-User`. |
| `SEERR_MCP_IDENTITIES` | Path to the identities file below |
| `SEERR_MCP_NOTIFY_SECRET` | The value Seerr's webhook sends in `X-Seerr-Mcp-Secret` |
| `HERMES_WEBHOOK_BASE` | Hermes's webhook listener, e.g. `http://hermes:8644` |
| `HERMES_WEBHOOK_SECRET` | The HMAC key for Hermes's V2 webhook signature |
| `SEERR_MCP_ADDR` | Optional, default `0.0.0.0:8080` |
| `RUST_LOG` | Optional, default `info` |

The identities file has one entry per person. See [`examples/`](examples/) for a complete household.

```yaml
- profile: alice                  # a slug, [a-z0-9_-]{1,32}; also Hermes's profile name
  token_sha256: "<64 hex>"        # printf %s "$TOKEN" | sha256sum; the token itself is never stored
  seerr_user_id: 2                # Seerr → Users → the user's id; 0 is refused
  jellyfin_user_id: "0f1e2d3c..." # any case, with or without dashes
  matrix_room_id: "!room:example.org"  # optional; without it /notify skips this person
  language: fr                    # fr | en
```

Profiles, tokens and Jellyfin ids must be unique. `language` sets the language of the titles a person
sees first and of every message written for them; the other supported language is shown as the second
title. The supported languages are French and English, because every message is translated into both.

In Seerr, enable the webhook notification agent (Settings → Notifications → Webhook) for the
"Request Available" type only:

- Webhook URL: `http://<seerr-mcp>:8080/notify`
- Authorization header: empty
- A custom header `X-Seerr-Mcp-Secret` set to `SEERR_MCP_NOTIFY_SECRET`
- JSON payload: [`examples/seerr-webhook.json`](examples/seerr-webhook.json), the three fields
  `/notify` reads.

## Running it

```bash
docker run -d --name seerr-mcp \
  --env-file examples/seerr-mcp.env \
  -v "$PWD/examples/identities.yml:/config/identities.yml:ro" \
  -e SEERR_MCP_IDENTITIES=/config/identities.yml \
  ghcr.io/dlepaux/seerr-mcp:latest
```

The published image is `linux/arm64`. On another architecture, build it from the `Dockerfile`.
`seerr-mcp --version` prints the version without reading any configuration, so a deploy can smoke-test
a new image.

## Security model

- **A bearer token is an identity.** `/mcp` hashes the bearer with SHA-256 and compares it in constant
  time against each person's `token_sha256`. A missing or unknown token is a 401 before any MCP session
  starts. The identities file holds only hashes.
- **No tool can act as someone else.** The Seerr user comes from the token's entry, never from a tool
  argument, and every Seerr call carries `X-API-User`, because Seerr runs a call without it as the
  admin. A Seerr user id of 0 is refused at startup.
- **Nothing is requested without a yes.** `request_media` files only after the person accepts the
  confirmation form. A declined, unanswered or late answer files nothing.
- **`/notify` is authenticated** by a shared secret compared in constant time, and its outgoing POST is
  signed with HMAC-SHA256 over `<timestamp>.<body>`.
- **Titles and display names are untrusted.** They come from TMDB and Seerr users, so they are stripped
  of control characters and truncated before the model sees them, and the server instructions tell the
  model to treat them as data.
- **What it never logs:** tokens or their hashes, the Seerr API key, either shared secret, search
  queries or titles. Logs carry the failure kind, an HTTP status, and on `/notify` failures the profile
  name.
- **It refuses to start open.** Any required variable unset or empty, or an invalid identities file,
  stops the process before it binds.

Report a vulnerability as [security.md](security.md) describes.

## Metrics

Every counter carries a `kind` label, and every kind is registered at zero so an alert can read it
before the first event.

| Counter | Kinds |
|---------|-------|
| `seerr_mcp_seerr_failures_total` | `unreachable`, `http_5xx`, `permission_denied`, `unexpected` |
| `seerr_mcp_request_failures_total` | `elicitation_unsupported`, `timeout` |
| `seerr_mcp_auth_failures_total` | `mcp_token`, `notify_secret` |
| `seerr_mcp_notify_failures_total` | `hermes_rejected`, `hermes_unreachable`, `bad_payload` |
| `seerr_mcp_request_skipped_total` | `declined`, `quota`, `duplicate`, `already_available` |
| `seerr_mcp_notify_skipped_total` | `unmapped_user` |

The `_failures_total` counters are faults worth an alert; the `_skipped_total` ones are user outcomes.

## Development

```bash
cargo fmt --all -- --check && cargo clippy --all-targets -- -D warnings && cargo test
```

Seerr and Hermes are wiremock servers inside the tests, so the suite needs nothing external.
Releases are semantic-release on `main`: conventional commits decide the version, and a release builds
`ghcr.io/dlepaux/seerr-mcp` for linux/arm64.

## Contributing

See [contributing.md](contributing.md) and the [code of conduct](code-of-conduct.md). Releases are
listed in [changelog.md](changelog.md).

## License

[MIT](LICENSE)
