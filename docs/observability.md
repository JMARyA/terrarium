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

Metrics do not expose:

- Terraform state contents
- resource addresses or resource names
- output names or output values
- usernames
- token IDs or token values
- webhook URLs
- workspace names
- raw provider package paths

Default labels are bounded and low-cardinality, for example:

```text
result="ok|error|conflict|forbidden|not_found|locked"
archived="true|false"
operation="plan|apply|destroy|refresh|unknown|other"
route="/state/{*name}"
```

Terrarium parses Terraform state JSON only to produce aggregate counts, such as resource totals and output totals. Object identities are not exported.

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

Import it into Grafana and select your Prometheus datasource.

The dashboard is organized around:

1. **Overview**
   - active states
   - total resources
   - total storage
   - active locks
   - API error ratio

2. **State Change Velocity**
   - state pushes
   - state pulls
   - version creation rate
   - state byte deltas
   - failed writes

3. **Terraform Resource Inventory**
   - resources
   - managed vs data resources
   - resource instances
   - outputs and sensitive outputs
   - positive/negative resource deltas

4. **Storage**
   - current state bytes
   - version history bytes
   - registry bytes
   - total data directory bytes
   - versions per state

5. **Locks**
   - active locks
   - oldest lock age
   - lock conflicts
   - acquire/release rate

6. **Integrations**
   - webhook failures
   - webhook retries
   - provider mirror status
   - registry downloads/uploads

7. **Terrarium API Health**
   - request rate
   - error rate
   - p95 latency
   - uptime

## Metrics reference

### State inventory and lifecycle

| Metric | Type | Labels | Description |
| --- | --- | --- | --- |
| `terrarium_states_total` | gauge | `archived` | Number of current states. |
| `terrarium_state_creations_total` | counter | none | Number of states created for the first time. |
| `terrarium_state_deletions_total` | counter | `result` | State delete attempts. |
| `terrarium_state_archives_total` | counter | `action`, `result` | Archive/unarchive attempts. |
| `terrarium_state_versions_total` | gauge | none | Total retained state versions. |
| `terrarium_state_version_creations_total` | counter | none | Number of new retained versions written. |

### State change velocity

| Metric | Type | Labels | Description |
| --- | --- | --- | --- |
| `terrarium_state_pushes_total` | counter | `result` | State push attempts. |
| `terrarium_state_pulls_total` | counter | `result` | State pull attempts. |
| `terrarium_state_push_bytes` | histogram | none | Size of pushed state blobs. |
| `terrarium_state_pull_bytes` | histogram | none | Size of pulled state blobs. |
| `terrarium_state_size_bytes` | histogram | none | Distribution of current state sizes. |
| `terrarium_state_change_bytes` | histogram | none | Absolute byte delta between previous and new state version. |

Useful queries:

```promql
sum(rate(terrarium_state_pushes_total{result="ok"}[$__rate_interval]))
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
| `terrarium_tf_resources_total` | gauge | none | Total Terraform resources across current states. |
| `terrarium_tf_resources_by_mode_total` | gauge | `mode` | Resources by `managed` or `data`. |
| `terrarium_tf_resource_instances_total` | gauge | none | Total resource instances across current states. |
| `terrarium_tf_outputs_total` | gauge | none | Total outputs across current states. |
| `terrarium_tf_sensitive_outputs_total` | gauge | none | Total sensitive outputs across current states. |
| `terrarium_tf_resource_delta` | histogram | `direction` | Resource count delta per successful state push. |
| `terrarium_tf_instance_delta` | histogram | `direction` | Instance count delta per successful state push. |
| `terrarium_tf_output_delta` | histogram | `direction` | Output count delta per successful state push. |

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
| `terrarium_state_versions_per_state` | histogram | none | Distribution of retained versions per state. |

### Locks

| Metric | Type | Labels | Description |
| --- | --- | --- | --- |
| `terrarium_locks_active` | gauge | none | Number of active locks. |
| `terrarium_lock_acquires_total` | counter | `result`, `operation` | Lock acquire attempts. |
| `terrarium_lock_releases_total` | counter | `result` | Unlock attempts. |
| `terrarium_lock_conflicts_total` | counter | none | Lock acquire conflicts. |
| `terrarium_lock_age_seconds` | histogram | none | Lock age observed when a lock is released. |
| `terrarium_lock_max_age_seconds` | gauge | none | Age of the oldest active lock. |

Allowed `operation` values:

```text
plan | apply | destroy | refresh | unknown | other
```

### Webhooks

| Metric | Type | Labels | Description |
| --- | --- | --- | --- |
| `terrarium_webhooks_registered` | gauge | none | Registered webhooks. |
| `terrarium_webhook_deliveries_total` | counter | `event`, `result` | Webhook delivery attempts. |
| `terrarium_webhook_delivery_duration_seconds` | histogram | `event`, `result` | Webhook delivery latency. |
| `terrarium_webhook_retries_total` | counter | `event` | Webhook retry attempts. |

Webhook metrics are labeled by Terrarium event name, not webhook URL.

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
| `terrarium_registry_downloads_total` | counter | `result` | Provider binary downloads. |
| `terrarium_registry_uploads_total` | counter | `result` | Provider uploads. |
| `terrarium_registry_mirror_runs_total` | counter | `result` | Upstream mirror runs. |
| `terrarium_registry_mirror_running` | gauge | none | `1` while a mirror run is active, else `0`. |
| `terrarium_registry_mirror_last_success_timestamp_seconds` | gauge | none | Unix timestamp of last successful mirror sync. |

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
```
