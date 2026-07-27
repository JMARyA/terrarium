# Observability

Terrarium can expose a Prometheus-compatible metrics endpoint and ships with a starter Grafana dashboard for operators.

The observability surface is intentionally focused on **Terrarium platform behavior**:

- state inventory and growth
- state change velocity
- aggregate Terraform resource/output counts
- storage growth
- lock health
- webhook delivery health
- provider registry and mirror jobs
- Terrarium API latency and errors

## Privacy and cardinality model

Terrarium metrics are designed to be safe for admin dashboards without leaking Terraform state details.

`/metrics` sits behind the same bearer-token auth boundary as the rest of the admin surface (the web UI, the API), so it carries **workspace names** and **provider identifiers** (`namespace`/`type`) as labels — the same information any admin can already see by browsing `/w/{name}` or `/registry`. This is what makes it possible to answer operational questions like "which workspaces are busiest" or "which providers are actually used" directly from Prometheus/Grafana instead of building a separate tool.

**Tradeoff to be aware of:** because workspace/provider identity is now a label, a leaked metrics token exposes the full list of workspace and provider names and their activity in a single scrape — previously a leaked token only exposed aggregate counts. Treat the metrics token with the same care as an admin credential, and prefer `TERRARIUM_METRICS_TOKEN_FILE` + restricting `/metrics` at the network/reverse-proxy layer (see below).

Metrics still do not expose:

- Terraform state contents
- resource addresses or resource names
- output names or output values
- usernames
- token IDs or token values
- webhook URLs
- raw provider package paths

Default labels are bounded and low-cardinality, for example:

```text
result="ok|error|conflict|forbidden|not_found|locked"
archived="true|false"
operation="plan|apply|destroy|refresh|unknown|other"
route="/state/{*name}"
workspace="infra/prod"
namespace="hashicorp" type="aws"
```

Terrarium parses Terraform state JSON only to produce aggregate counts, such as resource totals and output totals. Object identities (resource addresses, output names/values) are never exported, only counts per workspace.

HTTP request-level metrics (`terrarium_http_requests_total`, `terrarium_http_request_duration_seconds`, and the request/response body size histograms) are deliberately **not** labeled with `workspace` — they fire on every request, so combined with `method`/`route`/`status` that would multiply cardinality by request volume rather than by event count. Per-workspace push/pull/lock counters already cover "how busy is this workspace" without that cost.

## Enabling metrics

Metrics are disabled by default.

Enable them with:

```shell
TERRARIUM_METRICS=1
```

The metrics endpoint requires a dedicated bearer token. Prefer using a token file:

```shell
TERRARIUM_METRICS_TOKEN_FILE=/run/secrets/terrarium_metrics_token
```

Alternatively:

```shell
TERRARIUM_METRICS_TOKEN='long-random-token'
```

When enabled, metrics are available at:

```text
GET /metrics
Authorization: Bearer <metrics-token>
```

If metrics are enabled but no token is configured, `/metrics` returns `503` instead of becoming public.

## Prometheus scrape config

Example:

```yaml
scrape_configs:
  - job_name: terrarium
    scheme: https
    metrics_path: /metrics
    static_configs:
      - targets: ["terrarium.example.com"]
    authorization:
      type: Bearer
      credentials_file: /etc/prometheus/secrets/terrarium_metrics_token
```

For defense in depth, also restrict `/metrics` to Prometheus at the network or reverse-proxy layer.

## Docker Compose example

```yaml
services:
  terrarium:
    image: git.hydrar.de/jmarya/terrarium:latest
    ports:
      - "8080:8080"
    volumes:
      - ./data:/app
    environment:
      - RUST_LOG=info
      - TERRARIUM_DATA=/app
      - TERRARIUM_METRICS=1
      - TERRARIUM_METRICS_TOKEN_FILE=/run/secrets/terrarium_metrics_token
    secrets:
      - terrarium_metrics_token
    command: "/bin/terra serve"

secrets:
  terrarium_metrics_token:
    file: ./secrets/terrarium_metrics_token
```

## Grafana dashboard

A starter dashboard is provided at:

```text
docs/grafana/terrarium-dashboard.json
```

Import it into Grafana and select your Prometheus datasource. A `workspace` template variable (multi-select, includes "All") lets you filter most panels down to a single workspace or a subset. Each section is a named, collapsible Grafana row so the dashboard can be skimmed by component.

The dashboard is organized around:

1. **Overview** — active states, total resources, total storage, active locks, API error ratio, oldest lock age.

2. **State Change Velocity** — state pushes, state pulls, version creation rate, state byte deltas.

3. **Terraform Resource Inventory** — resources, managed vs data resources, resource instances, outputs and sensitive outputs, positive/negative resource deltas.

4. **Storage** — current state bytes, version history bytes, registry bytes, total data directory bytes, versions per state.

5. **Locks** — lock acquire/release rate, conflicts, by result and operation.

6. **Integrations** — webhook failures and retries, provider mirror status, registry downloads/uploads, provider download popularity, time since last mirror success, last mirror run duration.

7. **Terrarium API Health** — request rate, error rate, per-route p95 latency, global p50/p90/p99 latency spread, uptime.

8. **Fleet Composition** — Terraform/OpenTofu version distribution across states, resources by provider, top resource types. Answers "what is my estate actually made of" — which versions are in use, which providers are load-bearing, what kinds of resources dominate.

9. **Authentication & Security** — user count, active user sessions vs API tokens, login attempt rate, failed-login rate (brute-force signal), token create/revoke, CSRF rejections, password changes.

10. **Per-Workspace Insights** — two complementary views:
   - **Rankings**, across all workspaces at once: busiest workspaces (push rate), stale workspaces (time since last activity), largest workspaces (current state size), lock activity by workspace.
   - **Per-Workspace Trends** — consolidated multi-series graphs (state size, resource count, state serial, push rate) where **each workspace is a single series inside a shared panel**, capped at the top 20 by value via `topk`. This deliberately does *not* render one panel per workspace: adding workspaces adds lines to existing graphs, so the panel count stays fixed no matter how large the fleet grows. Each panel's legend is a sortable table showing the last and max value per workspace — sort by it to rank — and the `$workspace` variable narrows the set when you want to focus.

## Metrics reference

### State inventory and lifecycle

| Metric | Type | Labels | Description |
| --- | --- | --- | --- |
| `terrarium_states_total` | gauge | `archived` | Number of current states. |
| `terrarium_state_creations_total` | counter | `workspace` | Number of states created for the first time. |
| `terrarium_state_deletions_total` | counter | `workspace`, `result` | State delete attempts. |
| `terrarium_state_archives_total` | counter | `workspace`, `action`, `result` | Archive/unarchive attempts. |
| `terrarium_state_versions_total` | gauge | none | Total retained state versions. |
| `terrarium_state_version_creations_total` | counter | `workspace` | Number of new retained versions written. |
| `terrarium_state_last_activity_timestamp_seconds` | gauge | `workspace` | Unix timestamp of the most recent push to this workspace. Backfilled from version file mtimes on startup. |
| `terrarium_state_serial` | gauge | `workspace` | Terraform/OpenTofu's own write-counter (`serial` field) for this workspace's current state. A useful signal for concurrent-modification drift. |

### State change velocity

| Metric | Type | Labels | Description |
| --- | --- | --- | --- |
| `terrarium_state_pushes_total` | counter | `workspace`, `result` | State push attempts. |
| `terrarium_state_pulls_total` | counter | `workspace`, `result` | State pull attempts. |
| `terrarium_state_push_bytes` | histogram | `workspace` | Size of pushed state blobs. |
| `terrarium_state_pull_bytes` | histogram | `workspace` | Size of pulled state blobs. |
| `terrarium_state_size_bytes` | histogram | none | Distribution of current state sizes, aggregate only. |
| `terrarium_state_current_bytes` | gauge | `workspace` | Current size of a single workspace's state. |
| `terrarium_state_change_bytes` | histogram | `workspace` | Absolute byte delta between previous and new state version. |

Useful queries:

```promql
sum(rate(terrarium_state_pushes_total{result="ok"}[$__rate_interval]))
```

```promql
topk(10, sum by (workspace) (rate(terrarium_state_pushes_total{result="ok"}[$__rate_interval])))
```

```promql
time() - terrarium_state_last_activity_timestamp_seconds > 30 * 24 * 3600
```

```promql
increase(terrarium_state_version_creations_total[24h])
```

```promql
histogram_quantile(0.95, sum(rate(terrarium_state_change_bytes_bucket[$__rate_interval])) by (le))
```

### Terraform resource inventory and deltas

| Metric | Type | Labels | Description |
| --- | --- | --- | --- |
| `terrarium_tf_resources_total` | gauge | `workspace` | Terraform resources in this workspace's current state. Use `sum(...)` for the platform total. |
| `terrarium_tf_resources_by_mode_total` | gauge | `mode` | Resources by `managed` or `data`, aggregate only. |
| `terrarium_tf_resource_instances_total` | gauge | `workspace` | Resource instances in this workspace's current state. |
| `terrarium_tf_outputs_total` | gauge | `workspace` | Outputs in this workspace's current state. |
| `terrarium_tf_sensitive_outputs_total` | gauge | none | Total sensitive outputs across current states, aggregate only. |
| `terrarium_tf_resource_delta` | histogram | `workspace`, `direction` | Resource count delta per successful state push. |
| `terrarium_tf_instance_delta` | histogram | `workspace`, `direction` | Instance count delta per successful state push. |
| `terrarium_tf_output_delta` | histogram | `workspace`, `direction` | Output count delta per successful state push. |

`direction` is one of:

```text
positive | negative | zero
```

Delta histograms record the absolute delta value and use `direction` to distinguish growth from shrinkage.

### Storage

| Metric | Type | Labels | Description |
| --- | --- | --- | --- |
| `terrarium_storage_current_state_bytes` | gauge | none | Bytes used by current state blobs. |
| `terrarium_storage_state_versions_bytes` | gauge | none | Bytes used by retained version history. |
| `terrarium_storage_locks_bytes` | gauge | none | Bytes used by lock audit files. |
| `terrarium_storage_registry_bytes` | gauge | none | Bytes used by provider registry storage. |
| `terrarium_storage_total_bytes` | gauge | none | Total Terrarium data directory usage. |
| `terrarium_state_versions_per_state` | histogram | none | Distribution of retained versions per state, aggregate only. |

### Locks

| Metric | Type | Labels | Description |
| --- | --- | --- | --- |
| `terrarium_locks_active` | gauge | none | Number of active locks. |
| `terrarium_lock_acquires_total` | counter | `workspace`, `result`, `operation` | Lock acquire attempts. |
| `terrarium_lock_releases_total` | counter | `workspace`, `result` | Unlock attempts. |
| `terrarium_lock_conflicts_total` | counter | `workspace` | Lock acquire conflicts. |
| `terrarium_lock_age_seconds` | histogram | `workspace` | Lock age observed when a lock is released. |
| `terrarium_lock_max_age_seconds` | gauge | none | Age of the oldest active lock, aggregate only. |

Allowed `operation` values:

```text
plan | apply | destroy | refresh | unknown | other
```

### Webhooks

| Metric | Type | Labels | Description |
| --- | --- | --- | --- |
| `terrarium_webhooks_registered` | gauge | none | Registered webhooks. |
| `terrarium_webhook_deliveries_total` | counter | `workspace`, `event`, `result` | Webhook delivery attempts. |
| `terrarium_webhook_delivery_duration_seconds` | histogram | `workspace`, `event`, `result` | Webhook delivery latency. |
| `terrarium_webhook_retries_total` | counter | `workspace`, `event` | Webhook retry attempts. |

Webhook metrics are labeled by workspace and Terrarium event name, never by webhook URL.

Known events include:

```text
state.push | state.delete | state.archive | state.unarchive | lock.acquire | lock.release
```

### Provider registry and mirror

| Metric | Type | Labels | Description |
| --- | --- | --- | --- |
| `terrarium_registry_providers_total` | gauge | none | Stored provider namespace/type pairs. |
| `terrarium_registry_versions_total` | gauge | none | Stored provider versions. |
| `terrarium_registry_platform_archives_total` | gauge | none | Stored provider platform archives. |
| `terrarium_registry_downloads_total` | counter | `namespace`, `type`, `result` | Provider binary downloads. |
| `terrarium_registry_uploads_total` | counter | `namespace`, `type`, `result` | Provider uploads. |
| `terrarium_registry_mirror_runs_total` | counter | `result` | Upstream mirror runs, aggregate only. |
| `terrarium_registry_mirror_running` | gauge | none | `1` while a mirror run is active, else `0`. |
| `terrarium_registry_mirror_last_success_timestamp_seconds` | gauge | none | Unix timestamp of last successful mirror sync. |
| `terrarium_registry_mirror_last_duration_seconds` | gauge | none | Wall-clock duration of the most recent mirror run. |

```promql
topk(10, sum by (namespace, type) (rate(terrarium_registry_downloads_total{result="ok"}[$__rate_interval])))
```

Mirror staleness (alert if the last successful sync is too old):

```promql
time() - terrarium_registry_mirror_last_success_timestamp_seconds
```

### Fleet composition

Aggregate-only distributions across all current states — keyed by version / provider / resource-type, **never** by workspace, so label cardinality is bounded by how many distinct versions/providers/types exist rather than by workspace count.

| Metric | Type | Labels | Description |
| --- | --- | --- | --- |
| `terrarium_tf_version_states` | gauge | `version` | Number of current states on each Terraform/OpenTofu version. |
| `terrarium_tf_provider_resources` | gauge | `provider` | Resource count per provider (short `namespace/name`) across the fleet. |
| `terrarium_tf_resource_type_total` | gauge | `type` | Resource count per resource type across the fleet. |

Provider labels are normalized to a short `namespace/name` (e.g. `hashicorp/aws`); the full registry-source URL is not exported.

```promql
topk(15, terrarium_tf_resource_type_total)
```

### Authentication and security

All auth metrics are aggregate — no usernames, token IDs, or session IDs are ever used as labels.

| Metric | Type | Labels | Description |
| --- | --- | --- | --- |
| `terrarium_auth_logins_total` | counter | `result` | Web login attempts (`success` / `failure`). |
| `terrarium_auth_sessions_active` | gauge | `kind` | Active sessions by `kind` (`user` = browser login, `api` = API token). |
| `terrarium_users_total` | gauge | none | Number of registered users. |
| `terrarium_auth_token_operations_total` | counter | `action` | API token lifecycle (`create` / `revoke`). |
| `terrarium_auth_csrf_failures_total` | counter | `action` | Rejected requests due to a bad CSRF token, by originating action. |
| `terrarium_auth_password_changes_total` | counter | `result` | Self-service password changes (`ok` / `error`). |

```promql
sum(rate(terrarium_auth_logins_total{result="failure"}[$__rate_interval]))
```

### Terrarium API health

| Metric | Type | Labels | Description |
| --- | --- | --- | --- |
| `terrarium_http_requests_total` | counter | `method`, `route`, `status` | HTTP requests by normalized route. |
| `terrarium_http_request_duration_seconds` | histogram | `method`, `route`, `status` | Request latency. |
| `terrarium_http_request_body_bytes` | histogram | `route` | Request body sizes from `Content-Length` when present. |
| `terrarium_http_response_body_bytes` | histogram | `route` | Response body sizes from `Content-Length` when present. |
| `terrarium_build_info` | gauge | `version` | Build/version info, value always `1`. |
| `terrarium_uptime_seconds` | gauge | none | Server uptime. |

## Recommended alerts

```yaml
groups:
  - name: terrarium
    rules:
      - alert: TerrariumHighErrorRate
        expr: |
          sum(rate(terrarium_http_requests_total{status=~"5.."}[5m]))
          /
          sum(rate(terrarium_http_requests_total[5m])) > 0.05
        for: 10m
        labels:
          severity: warning
        annotations:
          summary: Terrarium HTTP 5xx rate is above 5%

      - alert: TerrariumStateWritesFailing
        expr: sum(rate(terrarium_state_pushes_total{result!="ok"}[10m])) > 0
        for: 15m
        labels:
          severity: warning
        annotations:
          summary: Terrarium state writes are failing

      - alert: TerrariumLockStuck
        expr: terrarium_lock_max_age_seconds > 3600
        for: 15m
        labels:
          severity: warning
        annotations:
          summary: A Terrarium lock has been active for over one hour

      - alert: TerrariumStorageGrowingQuickly
        expr: increase(terrarium_storage_total_bytes[24h]) > 10737418240
        for: 0m
        labels:
          severity: info
        annotations:
          summary: Terrarium storage grew by more than 10GiB in 24h

      - alert: TerrariumWebhookFailures
        expr: sum(rate(terrarium_webhook_deliveries_total{result!="ok"}[10m])) > 0
        for: 15m
        labels:
          severity: warning
        annotations:
          summary: Terrarium webhook delivery failures detected

      - alert: TerrariumMirrorFailing
        expr: increase(terrarium_registry_mirror_runs_total{result="error"}[1h]) > 0
        for: 0m
        labels:
          severity: warning
        annotations:
          summary: Terrarium provider mirror has failed

      - alert: TerrariumMirrorStale
        expr: time() - terrarium_registry_mirror_last_success_timestamp_seconds > 259200
        for: 0m
        labels:
          severity: warning
        annotations:
          summary: Terrarium provider mirror has not succeeded in over 3 days

      - alert: TerrariumLoginBruteForce
        expr: sum(rate(terrarium_auth_logins_total{result="failure"}[5m])) > 1
        for: 10m
        labels:
          severity: warning
        annotations:
          summary: Sustained failed-login rate against Terrarium (possible brute-force)
```
