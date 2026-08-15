# evolve-gateway

A reusable GraphQL federation gateway image: a custom
[Hive Router](https://the-guild.dev/graphql/hive/docs/router) binary with the
`federated_token` plugin compiled in, plus a standard router config driven
entirely from the environment.

```
ghcr.io/evolve-platform/evolve-gateway
```

Hive Router's plugin model is "bring your own binary" — plugins are Rust and are
compiled into the router. This repository exists so that binary is built and
released **once**, and every project consumes the same image instead of carrying
its own Rust build.

## Quick start

```bash
docker run --rm -p 4000:4000 \
  -e JWT_ISSUER=https://api.example.com \
  -e JWT_AUDIENCE=https://api.example.com \
  -e COOKIE_DOMAIN=.example.com \
  -e CORS_ORIGINS='["https://www.example.com"]' \
  -e FEDERATED_TOKEN_ENCRYPT_KEY_1="$ENCRYPT_KEY" \
  -e FEDERATED_TOKEN_SIGN_KEY_1="$SIGN_KEY" \
  -e HIVE_CDN_ENDPOINT="$HIVE_CDN_ENDPOINT" \
  -e HIVE_CDN_KEY="$HIVE_CDN_KEY" \
  ghcr.io/evolve-platform/evolve-gateway:latest
```

Or, to see it boot with no registry at all:

```bash
docker compose -f examples/docker-compose.yaml up
curl localhost:4000/health
```

Pin a released tag (`:1.2.3` or `:1.2`) in production — `:latest` moves.

## How configuration works

Hive Router does not interpolate `${VAR}` in its YAML; it only honours a small
fixed set of built-in env overrides. So the binary renders the config file as a
template before the router reads it:

| Form                | Meaning                                          |
| ------------------- | ------------------------------------------------ |
| `${NAME}`           | required — **startup fails** if unset             |
| `${NAME:-default}`  | `default` when unset or empty                     |
| `${NAME-default}`   | `default` when unset (an empty value stays empty) |
| `$${NAME}`          | a literal `${NAME}`                               |

A `$` that is not followed by `{` is left alone, so VRL expressions pass through
untouched. Rendering is plain text and does not understand YAML comments — a
placeholder inside a comment is substituted too, so escape those.

A missing required variable aborts the boot and reports **every** missing name at
once, rather than starting with an empty secret whose tokens silently fail to
verify.

### Bringing your own config

The image ships the standard config at `/app/router.config.yaml`. To replace it:

```bash
docker run --rm -p 4000:4000 \
  -v ./my-router.config.yaml:/config/router.config.yaml:ro \
  -e ROUTER_CONFIG_FILE_PATH=/config/router.config.yaml \
  ... ghcr.io/evolve-platform/evolve-gateway:latest
```

Templating applies to your file the same way, so you get `${VAR}` support in any
config — something upstream Hive Router does not offer.

The rendered result is written next to the source file (`.router.config.rendered.yaml`,
mode `0600`), because Hive Router resolves relative paths in the config against
the config's own directory. If that directory is read-only — a Kubernetes
ConfigMap volume, for instance — the gateway falls back to a temp directory and
warns; in that case use `HIVE_CDN_ENDPOINT` or an absolute `SUPERGRAPH_FILE_PATH`
rather than a relative path.

## Supergraph

Not baked into the image. Pick one:

| Source        | Variables                                                 |
| ------------- | --------------------------------------------------------- |
| Hive CDN      | `HIVE_CDN_ENDPOINT` + `HIVE_CDN_KEY` (polled for changes)   |
| Mounted file  | `SUPERGRAPH_FILE_PATH`                                      |

Setting both is an error. Composition is each project's own concern; this repo
does not compose supergraphs.

## Environment variables

### Required

| Variable                        | Notes                                            |
| ------------------------------- | ------------------------------------------------ |
| `JWT_ISSUER`                    | `iss` claim, validated on every token             |
| `JWT_AUDIENCE`                  | `aud` claim, validated on every token             |
| `COOKIE_DOMAIN`                 | Domain for the auth cookies                       |
| `FEDERATED_TOKEN_ENCRYPT_KEY_1` | JWE key — **exactly 32 bytes**                    |
| `FEDERATED_TOKEN_SIGN_KEY_1`    | JWS key — **exactly 32 bytes**                    |

Plus a supergraph source (see above). There are deliberately **no default
secrets**: an image shared across projects must not ship keys.

### CORS

| Variable        | Default | Notes                                                          |
| --------------- | ------- | -------------------------------------------------------------- |
| `CORS_ENABLED`  | `true`  |                                                                  |
| `CORS_ORIGINS`  | `[]`    | JSON array string, e.g. `'["https://a.com","https://b.com"]'`     |

Credentialed requests require explicit origins, never `*`. Terraform can produce
the value with `jsonencode(...)`. The default of no origins blocks cross-origin
browser traffic until a project opts in.

### Persisted / trusted documents

| Variable                         | Default | Notes                                                        |
| -------------------------------- | ------- | ------------------------------------------------------------ |
| `PERSISTED_DOCUMENTS_ENABLED`    | `false` | Enforces documentIds registered in the Hive CDN               |
| `TRUSTED_DOCUMENT_BYPASS_SECRET` | unset   | Per-request escape hatch; unset means the bypass is closed    |

Storage reuses `HIVE_CDN_ENDPOINT` / `HIVE_CDN_KEY`. When the bypass secret is
set, a request sending it as `X-TrustedDocument-BypassSecret` may send arbitrary
operations.

### Telemetry

| Variable                          | Default                  | Notes                                    |
| --------------------------------- | ------------------------ | ---------------------------------------- |
| `OTEL_TRACES_ENABLED`             | `false`                  |                                          |
| `OTEL_METRICS_ENABLED`            | `false`                  |                                          |
| `OTEL_EXPORTER_OTLP_ENDPOINT`     | `http://localhost:4318`  | `/v1/traces` and `/v1/metrics` appended  |
| `OTEL_EXPORTER_OTLP_HEADER_NAME`  | `x-honeycomb-team`       | Collector auth header                    |
| `OTEL_EXPORTER_OTLP_HEADER_VALUE` | empty                    | e.g. the Honeycomb API key               |
| `OTEL_SERVICE_NAME`               | `evolve-gateway`         | `service.name` resource attribute        |
| `OTEL_PROPAGATE_TRACE_CONTEXT`    | `true`                   | W3C `traceparent`                        |
| `OTEL_PROPAGATE_B3`               | `false`                  |                                          |

OTLP is sent over HTTP rather than gRPC, because collectors are commonly fronted
by an L7 proxy that mangles gRPC HTTP/2 trailers.

W3C `traceparent` is the default propagator: a storefront on `@vercel/otel`
registers `tracecontext` and subgraphs take the OTel JS default, so turning it
off leaves the gateway unable to join either side and every trace ends up
single-service. Enable B3 (`OTEL_PROPAGATE_B3=true`) for a stack that runs
`OTEL_PROPAGATORS=b3` throughout — e.g. on GCP, where B3 avoids interference with
the spans Google's ingress creates from `x-cloud-trace-context`. Turn W3C off
there too if that ingress injects a `traceparent` whose root span is never
exported.

### Federated token plugin

| Variable                            | Default          |
| ----------------------------------- | ---------------- |
| `FEDERATED_TOKEN_ENABLED`           | `true`           |
| `FEDERATED_TOKEN_COOKIE_SECURE`     | `true`           |
| `FEDERATED_TOKEN_SAME_SITE`         | `lax`            |
| `FEDERATED_TOKEN_REFRESH_PATH`      | `/graphql/auth`  |
| `FEDERATED_TOKEN_USER_MAX_AGE`      | `172800` (2d)    |
| `FEDERATED_TOKEN_REFRESH_MAX_AGE`   | `17280000` (200d)|
| `FEDERATED_TOKEN_SUBJECT_KEY`       | `commercetools`  |
| `FEDERATED_TOKEN_ENCRYPT_KEY_ID_1`  | `1`              |
| `FEDERATED_TOKEN_SIGN_KEY_ID_1`     | `1`              |

To rotate keys, add a second key with a new id to the config — the first key in
each list signs new tokens while the rest still verify existing ones. The standard
config declares one of each; rotation needs your own config file.

### Hive Router built-ins

Applied by the router itself, after the config is parsed:
`HOST`, `PORT`, `LOG_LEVEL`, `LOG_FORMAT`, `LOG_FILTER`, `ROUTER_HTTP_WORKERS`,
`TELEMETRY_TRACING_SAMPLING_RATE`, `HIVE_CDN_POLL_INTERVAL`, `HIVE_ACCESS_TOKEN`,
`HIVE_TARGET`, `LABORATORY_ENABLED`, `WEBSOCKET_ENABLED`, `SUBSCRIPTIONS_ENABLED`.

## What the standard config does

- **CORS** with credentials and an explicit origin allowlist.
- **Header propagation** to subgraphs: `X-StoreContext-StoreKey` / `-Locale` /
  `-Currency` verbatim, and `X-Forwarded-For` renamed to `X-Client-IP` with a
  `127.0.0.1` default (subgraphs reject an empty `X-Client-IP`).
- **Persisted documents** against the Hive CDN, with a bypass-secret escape hatch.
- **OpenTelemetry** traces and metrics over OTLP/HTTP.
- **`federated_token`** — the auth plugin, below.

Anything else, bring your own config.

## The `federated_token` plugin

Ports the `@labdigital/federated-token` **gateway** behaviour off Apollo Server
without touching any subgraph. The wire format between gateway and subgraphs is
reproduced byte-for-byte, so subgraphs running the Node `FederatedAuthPlugin` are
unaffected. It lives in
[evolve-platform/hive-router-federated-token](https://github.com/evolve-platform/hive-router-federated-token)
and is pinned by tag in `Cargo.toml`.

- **Access token** — JWE, `dir` + `A256GCM`. Payload `{ tokens, isAuthenticated, iss, aud, iat, exp }`, `kid` in the protected header, `exp` = earliest per-subgraph token expiry.
- **Refresh token** — JWE, `dir` + `A256GCM`. Per-service refresh tokens as top-level claims, `exp` = now + 90 days.
- **Data token** — JWS, `HS256`. Payload `{ values, sub?, iss, aud, iat, exp }`, `exp` = now + 90 days.
- **Subgraph headers** — `x-access-token` / `x-refresh-token` carry **base64 (standard alphabet, not url-safe) of sparse JSON**, not JWTs.
- **Cookies** — `userToken`/`guestToken` (httpOnly), `userData`/`guestData` (readable), `refreshToken` (httpOnly, scoped to `FEDERATED_TOKEN_REFRESH_PATH`), `userRefreshTokenExists`/`guestRefreshTokenExists` = `"1"`.

Secret bytes are the raw UTF-8 of the configured string (matching Node's
`createSecretKey(Buffer.from(secret))`), so each key must be exactly 32 bytes.

## Development

Requires a Rust toolchain (`rustup`).

```bash
task dev            # run on :4000 against examples/supergraph.graphql
task test           # unit tests
task lint           # cargo fmt --check + clippy
task docker:build   # build the image
task docker:run     # build, then run it against the example supergraph
task changelog      # add a changelog fragment (see Releasing)
```

`task dev` supplies throwaway dev values for the required variables; put real
ones in `.env` to override.

Bumping Hive Router means bumping **both** git dependencies in `Cargo.toml` in
lockstep — the plugin crate compiles against a specific router version.

## Releasing

Versions come from [changie](https://github.com/miniscruff/changie), same as the
Terraform providers and `staticfiles-sync`. Every user-visible change carries a
changelog fragment:

```bash
task changelog      # or: changie new
```

That writes a YAML fragment to `.changes/unreleased/`; commit it with the change.
The `kind` you pick decides the bump — `Added`/`Deprecated` → minor,
`Changed`/`Removed` → major, `Fixed`/`Security`/`Dependency` → patch.

From there it is automatic:

1. Any push to `main` publishes `:main` and `:sha-<short>` (rolling, for tst),
   and refreshes an open **Release vX.Y.Z** PR containing the batched
   `CHANGELOG.md`.
2. Merging that PR tags the commit, creates the GitHub release, and publishes the
   versioned tags: `:1.2.3`, `:1.2`, `:latest` — which is what projects pin to.

Pushing a `v*.*.*` tag by hand publishes the same versioned tags, if you ever
need to release outside that flow.

Images are built on native amd64 and arm64 [Namespace](https://namespace.so)
runners and joined into one manifest — no QEMU, which the Rust compile is far too
slow under. arm64 is required, not optional: it is what developers pull on Apple
silicon, so CI builds and boots both architectures on every PR and the release
fails if the published manifest is missing either one.

## Migrating from the in-repo `backend/services/graphql-gateway`

This repo started as an extraction of that service. Behaviour is the same, but
the packaging changed:

- **No default token keys.** The old entrypoint fell back to the Apollo gateway's
  hard-coded keys. Set `FEDERATED_TOKEN_ENCRYPT_KEY_1` and
  `FEDERATED_TOKEN_SIGN_KEY_1` explicitly — with the previous values if existing
  sessions must survive the cutover.
- **`HONEYCOMB_API_KEY` → `OTEL_EXPORTER_OTLP_HEADER_VALUE`.**
- **Telemetry and persisted documents are now opt-in.** Set
  `OTEL_TRACES_ENABLED=true`, `OTEL_METRICS_ENABLED=true` and
  `PERSISTED_DOCUMENTS_ENABLED=true` to keep the previous behaviour.
- **No `envsubst`, no entrypoint script.** The binary renders the config, so the
  allowlist of substitutable variables is gone — every `${VAR}` is substituted.
- **Supergraph composition moved out.** It belongs to the consuming project.
