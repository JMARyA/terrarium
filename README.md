# 🌱 Terrarium

> **A safe enclosure for your Terraform state.** 🦎🪴

**Terrarium** is a small, boring, correct **Terraform HTTP state backend**.

It stores Terraform state as an opaque blob, provides strict locking, tracks full version history, and stays completely out of your way.

No S3. No Terraform Cloud. No vendor assumptions.

## Why?

Terraform state is critical, shared, and easy to corrupt. Terrarium exists because storing state in Git is unsafe, S3 should not be mandatory, and the Terraform HTTP backend deserves a first-class server.

## Features

- 🌱 Terraform-compatible HTTP backend (lock/unlock/push/pull)
- 🔒 Strict, explicit state locking
- 📜 Full version history — every push is versioned, diffs included
- 📦 Workspace archival — mark a workspace read-only permanently
- 🪝 Webhooks — event-driven integration per workspace
- 🗂 Path-prefix namespacing — `infra/prod`, `apps/backend/staging`
- 🦎 Single static binary
- 🧱 Cloud-agnostic
- 🔐 HTTP Basic Auth

---

## Deployment

### Docker Compose

```yaml
services:
  terrarium:
    image: git.hydrar.de/jmarya/terrarium:latest
    ports:
      - "8080:8080"
    volumes:
      - ./data:/app # all state, locks, users, versions and webhooks live here
    environment:
      - "RUST_LOG=info"
      - "TERRARIUM_DATA=/app"
    command: "/bin/terrarium serve"
```

All data is stored under `TERRARIUM_DATA` (`/app` in the container, `./data` on the host):

| Path            | Contents             |
| --------------- | -------------------- |
| `state/`        | Current state blobs  |
| `versions/`     | Full version history |
| `locks/`        | Persisted lock files |
| `users/`        | User database        |
| `webhooks.json` | Registered webhooks  |

---

## Terraform Configuration

```hcl
terraform {
  backend "http" {
    address        = "https://terrarium.example/state/infra/prod"
    lock_address   = "https://terrarium.example/lock/infra/prod"
    unlock_address = "https://terrarium.example/lock/infra/prod"

    lock_method   = "POST"
    unlock_method = "DELETE"
  }
}
```

Provide credentials via `$TF_HTTP_USERNAME` and `$TF_HTTP_PASSWORD`, then run `tofu init` to migrate.

Workspace names support path prefixes: `infra/prod`, `apps/backend/staging`, etc.

---

## User Management

User management commands act directly on the local user database. When running in Docker, exec into the container or they'll write to a different path than the server uses:

```shell
docker compose exec terrarium /bin/terrarium user add alice
```

```shell
# Add a user (prompts for password if omitted)
terrarium user add <username> [password]

# Change a user's password
terrarium user passwd <username>

# Delete a user
terrarium user delete <username>

# List all users
terrarium user list
```

---

## Client — Remote Operations

The `remote` subcommand talks to a running terrarium server over HTTP.

### Configuration

Credentials and server URL are loaded in priority order:

1. Environment variables: `TERRARIUM_URL`, `TERRARIUM_USER`, `TERRARIUM_PASSWORD`
2. Config file (first found wins):
   - `--config <path>` flag
   - `$TERRARIUM_CONFIG`
   - `~/.config/terrarium/config.toml`
   - `~/.terrarium.toml`
3. Interactive password prompt (if `TERRARIUM_PASSWORD` is unset)

Config file format (`~/.config/terrarium/config.toml`):

```toml
url = "https://terrarium.example"
username = "alice"
# password: use TERRARIUM_PASSWORD env var, not the config file
```

Pass `--config <path>` to any `remote` command to use a specific config file.

### State

```shell
# List all states
terrarium remote state list

# List states under a path prefix
terrarium remote state list infra/

# Get current state (pretty-printed JSON by default)
terrarium remote state get infra/prod

# Get raw JSON
terrarium remote state get infra/prod --raw

# Get a specific historical version
terrarium remote state get infra/prod --version 3

# List all available versions
terrarium remote state versions infra/prod

# Diff two versions (structural JSON diff)
terrarium remote state diff infra/prod 2 5

# Force-unlock a state
terrarium remote state unlock infra/prod

# Archive a state (permanently read-only — cannot be undone)
terrarium remote state archive infra/prod
```

### Locks

```shell
# List all active locks
terrarium remote lock list
```

### Self-Service User

```shell
# Change your own password
terrarium remote user passwd [new-password]
```

### Webhooks

Webhooks are scoped per workspace and fire on state and lock events.

```shell
# Register a webhook (all events)
terrarium remote webhook add infra/prod https://hooks.example.com/tf

# Register for specific events only
terrarium remote webhook add infra/prod https://hooks.example.com/tf \
  --events state.push,lock.acquire

# List webhooks for a workspace
terrarium remote webhook list infra/prod

# Remove a webhook by ID
terrarium remote webhook remove <id>
```

**Supported events:** `state.push`, `state.delete`, `state.archive`, `lock.acquire`, `lock.release`

**Payload:**

```json
{
  "event": "state.push",
  "workspace": "infra/prod",
  "version": 4,
  "user": "alice",
  "timestamp": "2026-03-29T16:00:00Z"
}
```

Delivery is retried up to 4 times with exponential backoff (1 s → 2 s → 4 s) before giving up.

---

## HTTP API

All endpoints require HTTP Basic Auth.

| Method   | Path                    | Description                                                |
| -------- | ----------------------- | ---------------------------------------------------------- |
| `GET`    | `/state`                | List all state names (`?prefix=infra/` to scope)           |
| `GET`    | `/state/{name}`         | Get current state (`?version=N` for history)               |
| `POST`   | `/state/{name}`         | Push state (`?ID=<lock-id>` if locked)                     |
| `DELETE` | `/state/{name}`         | Delete state                                               |
| `GET`    | `/versions/{name}`      | List version numbers for a state                           |
| `POST`   | `/archive/{name}`       | Archive a state (mark read-only)                           |
| `GET`    | `/lock`                 | List all active locks                                      |
| `POST`   | `/lock/{name}`          | Acquire lock                                               |
| `DELETE` | `/lock/{name}`          | Release lock                                               |
| `PUT`    | `/user/password`        | Change own password (`{ current_password, new_password }`) |
| `GET`    | `/webhooks/{workspace}` | List webhooks for workspace                                |
| `POST`   | `/webhooks/{workspace}` | Register webhook (`{ url, events[] }`)                     |
| `DELETE` | `/webhooks/id/{id}`     | Remove webhook by ID                                       |
