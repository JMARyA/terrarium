# Webhooks

Webhooks are registered per workspace and `POST` a JSON payload when something happens to it.

```shell
terra remote webhook add infra/prod https://hooks.example.com/tf --events state.push,lock.expire
# Webhook registered — ID: 3f0c…
# Signing secret: whsec_9a1e…
```

The signing secret is shown **once**, in the response that creates the hook. Store it in your receiver's configuration. If it is lost or leaked, replace it (the old secret stops working immediately):

```shell
terra remote webhook rotate-secret <id>
```

## Events

| Event | Fired when |
| --- | --- |
| `state.push` | A new state version is stored (`version` is set) |
| `state.delete` | A state is deleted |
| `state.archive` / `state.unarchive` | A state is archived or unarchived |
| `lock.acquire` | A lock is acquired |
| `lock.release` | A lock is released |
| `lock.expire` | A lock outlived `TERRARIUM_LOCK_TTL` and was taken over; `user` is who took it over |

Registration rejects unknown event names. Omitting `--events` subscribes to all of them.

## Payload

```json
{
  "event": "state.push",
  "workspace": "infra/prod",
  "version": 4,
  "user": "alice",
  "timestamp": "2026-03-29T16:00:00Z"
}
```

## Verifying deliveries

Every request carries:

| Header | Value |
| --- | --- |
| `X-Terrarium-Event` | The event name |
| `X-Terrarium-Delivery` | A UUID, identical across retries of the same delivery — use it to deduplicate |
| `X-Terrarium-Timestamp` | Unix seconds at send time |
| `X-Terrarium-Signature` | `sha256=` + hex HMAC-SHA256, keyed with the secret, over `"{timestamp}.{raw body}"` |

A receiver should:

1. Compute the HMAC over the **raw** request body (not re-serialized JSON).
2. Compare it to the header in constant time.
3. Reject timestamps more than a few minutes old, so a captured request can't be replayed.

```python
import hashlib, hmac, time

def verify(secret: str, headers, body: bytes, tolerance: int = 300) -> bool:
    timestamp = headers["X-Terrarium-Timestamp"]
    if abs(time.time() - int(timestamp)) > tolerance:
        return False
    expected = "sha256=" + hmac.new(
        secret.encode(), timestamp.encode() + b"." + body, hashlib.sha256
    ).hexdigest()
    return hmac.compare_digest(expected, headers["X-Terrarium-Signature"])
```

Hooks registered before signing existed are given a secret when the server starts. Their deliveries are signed from then on; run `rotate-secret` to obtain the secret.

## Delivery

- Each attempt times out after 10 s (5 s to connect). Redirects are not followed.
- Network errors, `5xx`, `408` and `429` are retried up to 4 attempts in total, with 1 s → 2 s → 4 s backoff. Other `4xx` responses are not retried.
- Deliveries are sent concurrently and are **not ordered**: `lock.expire` may arrive before or after the `lock.acquire` of the takeover. Order by the payload `timestamp` if it matters.
- Logs name only the receiver's scheme and host; webhook paths often embed tokens.

## Network restrictions

A webhook URL makes the *server* send requests, so Terrarium restricts where they can go with `TERRARIUM_WEBHOOK_NETWORKS`:

| Value | Allowed destinations |
| --- | --- |
| `public` | Public addresses only |
| `private` (default) | Public and private networks (RFC 1918, `100.64.0.0/10`, IPv6 ULA) |
| `any` | Everything, including loopback and cloud metadata endpoints |

Loopback, link-local (including `169.254.169.254`), `fd00:ec2::254`, unspecified, multicast and broadcast addresses are blocked unless the value is `any`.

Addresses are checked when a hook is registered, and again on every delivery by the resolver that dials the connection, so a hostname that later re-resolves to a blocked address (DNS rebinding) is still refused. Deliveries refused this way are counted as `result="blocked"` in `terrarium_webhook_deliveries_total`.

With `public` or `private`, webhook requests bypass `HTTP(S)_PROXY`, because a proxy resolves the destination itself and would sidestep the check. If deliveries must go through a proxy, set `any`.
