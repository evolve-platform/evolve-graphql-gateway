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

A value that spans several lines is re-indented: every line after the first gets
the indentation of the line its placeholder was on. That is what lets a whole VRL
program travel in an environment variable (see `SUBGRAPH_URL_EXPRESSION`) and still
land as valid YAML instead of breaking out of its block in column 0.

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

### Subgraph URL rewriting

| Variable                  | Default    | Notes                                                     |
| ------------------------- | ---------- | --------------------------------------------------------- |
| `SUBGRAPH_URL_EXPRESSION` | `.default` | VRL expression returning the URL to call, per subgraph request |

The supergraph pins every subgraph to one exact URL. This variable is the hook for
sending the request somewhere else instead; unset, it is `.default` and nothing is
rewritten. Nothing about any particular hosting setup is baked into the image — the
expression is yours.

It is compiled once, when the supergraph loads, and then evaluated on every
subgraph fetch — so a syntax error surfaces at load time, while the values below
are the ones from the request in flight:

| In scope         | Is                                                      |
| ---------------- | ------------------------------------------------------- |
| `.default`       | the URL from the supergraph                              |
| `.subgraph.name` | the subgraph being called                                |
| `.request`       | `method`, `headers`, `url`, `path_params`, `operation`, `jwt` |
| `env("NAME")`    | an environment variable, read at request time            |

It must return a string. Multi-line values are re-indented into the config, so a
real VRL program fits in the variable.

#### Blue/green on Azure Container Apps

ACA gives every revision a URL of its own (`app--0000065.<env>.<region>...`) and
every traffic label one too, with three dashes (`app---blue.<env>.<region>...`).
So a gateway staged on the `green` label reaches the green subgraphs by turning the
revision suffix in each supergraph URL into the label it is itself running on:

```hcl
env_vars = {
  # ...
  SUBGRAPH_URL_EXPRESSION = <<-EOT
    side = string!(env("EVOLVE_DEPLOY_SIDE", ""))
    if side == "" {
      .default
    } else {
      replace(string!(.default), r'^(https?://[^./]+?)--[^./]+\.', "$1---" + side + ".")
    }
  EOT
}
```

Reading `EVOLVE_DEPLOY_SIDE` with `env()` rather than interpolating it in Terraform
keeps one expression for both sides: the deploy tool sets the variable per revision
and the same config serves blue and green. With it unset, the rewrite is off, which
is what an environment that does not do label deployments wants.

Why that regex holds up: an ACA app name cannot contain `--`, so the first `--` in
the leading hostname label is always the revision separator. A URL without one is
left alone (a vanity hostname survives untouched), and re-running it on an
`---blue.` URL is a no-op.

Note that the gateway's side and a subgraph's side are separate labels on separate
apps. This only routes where you want it to if the deploy tool stages every service
onto the same side.

Caveats:

- The expression applies to **every** subgraph, subscriptions included. This image
  is shared across projects and cannot know the subgraph names, so per-subgraph
  routing means mounting your own config with `override_subgraph_urls.subgraphs`.
- `${...}` in the expression is substituted by the config template first, so write
  `$${...}` for a literal one. A lone `$` (`$1`, `$name` in a `replace()`) passes
  through untouched.
- A regex that matches nothing leaves the URL as it was — that is the failure mode
  to expect from a typo, rather than a broken endpoint.
- An expression that does not compile is fail-closed but **not** fail-fast: the
  supergraph never loads, so the gateway logs `Failed to compile VRL expression` at
  ERROR, keeps answering `/health` with 200, and answers every GraphQL request with
  a 503 `NO_SUPERGRAPH_AVAILABLE`. A rollout gated only on `/health` will not
  notice — gate on a real query, or watch the logs.
- Something the expression cannot express (per-subgraph URLs, several rules) is a
  reason to mount your own config with `ROUTER_CONFIG_FILE_PATH`. On ACA that means
  giving the container app a volume — an Azure Files share, or a secret mounted as
  a file — which is more moving parts than this variable, and the mount is
  read-only, so the rendered config lands in the temp-dir fallback (see *Bringing
  your own config*).

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
| `OTEL_EXPORTER_OTLP_HEADERS`      | empty                    | e.g. `x-honeycomb-team=<key>`            |
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

### Hive Console usage reporting

Reports executed operations and the schema fields they touch back to Hive
Console, which is what feeds the operations dashboard, unused-field detection and
usage-aware breaking-change checks.

| Variable                                    | Default                                | Notes                                                              |
| ------------------------------------------- | -------------------------------------- | ------------------------------------------------------------------ |
| `HIVE_USAGE_REPORTING_ENABLED`              | `false`                                |                                                                    |
| `HIVE_ACCESS_TOKEN`                         | unset                                  | Registry access token with **write** permission — required          |
| `HIVE_TARGET`                               | unset                                  | `$org/$project/$target` or a UUID — see below                        |
| `HIVE_USAGE_REPORTING_SAMPLING_RATE`        | `100%`                                 | Percentage string, `%` included                                     |
| `HIVE_USAGE_REPORTING_EXCLUDE_EXPRESSION`   | `false`                                | VRL; returning `true` drops the operation from the report           |
| `HIVE_USAGE_REPORTING_ENDPOINT`             | `https://app.graphql-hive.com/usage`   | Self-hosted Hive only                                              |

`HIVE_ACCESS_TOKEN` is a different credential from the `HIVE_CDN_KEY` that
fetches the supergraph — a CDN key cannot write usage. Enabling reporting without
it fails the boot with `Usage Reporting - missing access token`, rather than
running and dropping every report.

Whether `HIVE_TARGET` is needed is decided by the token's own prefix, and getting
it wrong fails the boot either way:

| Token                                  | `HIVE_TARGET`                                     |
| -------------------------------------- | ------------------------------------------------- |
| `hvo1/…`, `hvu1/…`, `hvp1/…` (current) | **required** — without it: `MissingTargetId`        |
| anything else (legacy registry token)  | **must be unset** — with it: `TargetIdWithLegacyToken` |

The exclude expression sees the same fields as `SUBGRAPH_URL_EXPRESSION` and is
compiled at startup, so a broken one fails the boot instead of a request. The
usual use is silencing noise:

```
HIVE_USAGE_REPORTING_EXCLUDE_EXPRESSION='.request.operation.name == "IntrospectionQuery"'
```

Client names in the dashboard come from the `graphql-client-name` and
`graphql-client-version` request headers, so operations only group by client if
the storefront sends them.

Sending traces to Hive Console instead of your own collector is a separate
switch, `HIVE_TRACING_ENABLED`; it is independent of the OTLP exporters above.

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
`HIVE_TARGET`, `HIVE_TRACING_ENABLED`, `HIVE_USAGE_REPORTING_ENABLED`,
`LABORATORY_ENABLED`, `WEBSOCKET_ENABLED`, `SUBSCRIPTIONS_ENABLED`.

## What the standard config does

- **CORS** with credentials and an explicit origin allowlist.
- **Header propagation** to subgraphs: `X-StoreContext-StoreKey` / `-Locale` /
  `-Currency` verbatim, and `X-Forwarded-For` renamed to `X-Client-IP` with a
  `127.0.0.1` default (subgraphs reject an empty `X-Client-IP`).
- **Subgraph URL rewriting**, off unless `SUBGRAPH_URL_EXPRESSION` is set.
- **Persisted documents** against the Hive CDN, with a bypass-secret escape hatch.
- **OpenTelemetry** traces and metrics over OTLP/HTTP.
- **Response compression** (gzip, br, zstd, deflate) negotiated via
  `Accept-Encoding` for responses of 1 KiB and up; compressed request bodies
  are accepted too. Built into Hive Router — no reverse proxy needed.
- **Hive Console usage reporting**, off unless `HIVE_USAGE_REPORTING_ENABLED=true`.
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
- **Telemetry and persisted documents are now opt-in.** Set
  `OTEL_TRACES_ENABLED=true`, `OTEL_METRICS_ENABLED=true` and
  `PERSISTED_DOCUMENTS_ENABLED=true` to keep the previous behaviour.
- **No `envsubst`, no entrypoint script.** The binary renders the config, so the
  allowlist of substitutable variables is gone — every `${VAR}` is substituted.
- **Supergraph composition moved out.** It belongs to the consuming project.
