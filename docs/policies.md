# Policies

Terrarium can check your infrastructure against [Rego](https://www.openpolicyagent.org/docs/latest/policy-language/) (OPA) policies at two points:

- **Before an apply** — `terra plan` reports violations and `terra apply` refuses to proceed on a `deny`.
- **After a push** — the server evaluates every state that lands and records what it finds, visible on the dashboard.

The Rego engine is built into `terra`. There is no OPA binary to install and no sidecar to run.

## What policy checks are, and are not

**The apply-time check is a guardrail, not access control.** Anyone who can push state can bypass it — with `--policy=off`, or by running `tofu` directly and never involving `terra` at all. That is a deliberate consequence of the server never rejecting a push, and it cannot be closed while the Terraform HTTP backend protocol stays what it is.

What does not depend on client cooperation is the **server-side record**: state is linted when it arrives, whatever the client did or didn't do. A bypassed check still shows up as a violation on the dashboard, attributed to whoever pushed it. Treat the reports as the source of truth and the apply-time gate as the thing that saves people from mistakes.

Note also that **any authenticated user can add, change, or delete policies**, including the ones that constrain them. Terrarium has no roles yet.

## Writing a policy

A policy declares one of two packages, which decides what it is given:

| Package | Input | Evaluated |
|---|---|---|
| `terrarium.plan` | `input.plan` — the output of `tofu show -json <planfile>` | client-side, before an apply |
| `terrarium.state` | `input.state` — the Terraform state as pushed | server-side, after a push |

Both also receive `input.workspace` and `input.user`.

Two rules are recognised, each a set of strings:

- `deny` — blocks an apply (unless the mode says otherwise)
- `warn` — reported, never blocks

```rego
package terrarium.plan

deny contains msg if {
    some rc in input.plan.resource_changes
    "delete" in rc.change.actions
    startswith(rc.address, "aws_db_instance.")
    msg := sprintf("refusing to destroy database %s", [rc.address])
}

warn contains msg if {
    some rc in input.plan.resource_changes
    "create" in rc.change.actions
    rc.type == "aws_s3_bucket"
    msg := sprintf("new bucket %s — check the access policy", [rc.address])
}
```

Policies use Rego v1 syntax. A policy declaring any other package is rejected when you push it, rather than being stored and silently never running.

## The workflow: author locally, then push

```
.terrarium/policies/*.rego   →   terra policy test   →   git commit   →   terra policy push
```

Policies live in your repository next to the Terraform they constrain. You iterate against a real plan with no server involved, commit the rule so it gets reviewed like any other change, then push it so your team and the server-side linter both pick it up.

```shell
tofu plan -out tfplan && tofu show -json tfplan > plan.json
terra policy test --input plan.json      # no server needed

terra policy push --dry-run              # what would change
terra policy push                        # publish to the team
```

Git is the version history — the server keeps only the current version of each policy, plus who last changed it and when.

### Repository policies are always in effect

`terra` looks for `.terrarium/policies/*.rego` by walking up from the working directory to the repository root, so it works from a nested module directory. Anything it finds is evaluated **in addition to** whatever the server sends.

This means a repository can add restrictions of its own, but cannot remove one the server sent — the server's bundle is fetched separately and no file in your repo can edit it. If a policy exists in both places with different content, both are evaluated and you get a warning telling you to reconcile them.

It also means **policy checking works with no server at all**. A local backend plus `.terrarium/policies/` gives you a complete, offline, version-controlled policy setup.

### Drift

Every `terra plan` and `terra apply` compares your repository against the server:

| Situation | What you see |
|---|---|
| Identical | nothing |
| Local policy not on the server | a note suggesting `terra policy push` |
| Server has policies your repo doesn't | nothing — global rules need not live in your repo |
| Same name, different content | **a warning**; both versions are evaluated |

`terra policy diff` runs the same comparison on demand.

## Controlling enforcement

`mode` decides what a `deny` does:

| Mode | Effect |
|---|---|
| `enforce` | `deny` blocks the apply (default) |
| `warn` | `deny` is printed but does not block |
| `off` | no evaluation at all |

Set it per invocation, or per workspace on the server:

```shell
terra apply --policy=warn
TERRARIUM_POLICY_MODE=off terra apply     # for CI
```

```jsonc
// data/policies/config.json, or PUT /policy/config
[
  { "scope": "",         "mode": "enforce", "lint": true },
  { "scope": "sandbox/", "mode": "warn",    "lint": true },
  { "scope": "infra/huge", "lint": false }
]
```

Scopes match the same way policy scopes do: `""` is global, a trailing slash is a path prefix, anything else is an exact workspace. The most specific match wins — exact, then longest prefix, then global.

Precedence, highest first:

```
--policy  >  TERRARIUM_POLICY_MODE  >  .terrarium/policy.json  >  server workspace config  >  server global  >  default
```

`.terrarium/policy.json` may only *raise* strictness. A repository can turn `warn` into `enforce`; it cannot turn `enforce` into `off`. Weakening has to happen on the command line, where it is visible in shell history and CI logs.

`terra policy config --workspace infra/prod` prints what is actually in force and where each setting came from.

## Server-side linting

Every state push is evaluated against the `terrarium.state` policies that apply to that workspace. This happens **after** the state is durably stored and after the response is sent, so it can never delay or fail a push — Terrarium never rejects a push for policy reasons.

Results appear on the dashboard and per workspace, and via the API:

```shell
curl -u user:pass https://terrarium/violations
curl -u user:pass https://terrarium/violations/infra/prod
```

A report is a snapshot, not a log: it is replaced on every push and cleared when the workspace comes back clean. A workspace that could not be evaluated (oversized state, a policy that timed out) is shown as **not checked** rather than as passing.

Set `lint: false` on a scope to skip linting for it, and `max_state_bytes` to change the size ceiling (32 MB by default).

## Where server policies come from

Two sources, both visible in the UI as the policy's *origin*:

- **`api`** — pushed with `terra policy push`. Editable and removable through the API.
- **`file`** — a `.rego` file placed directly in the server's `policies/` directory, e.g. by mounting it into the container. Loaded at startup and **read-only** through the API: the file is the operator's declared intent, so an API write to that name is refused rather than silently shadowing it.

## Reference

```shell
terra policy list                     # what the server has, with scope and origin
terra policy test --input plan.json   # evaluate local policies, no server
terra policy test --input state.json --state
terra policy push [--dry-run] [--workspace infra/prod]
terra policy pull [--workspace ...]   # seed .terrarium/policies from the server
terra policy diff                     # drift report
terra policy rm <name>
terra policy config [--workspace ...] # effective settings and their source
```

| Endpoint | Purpose |
|---|---|
| `GET /policy` | List policy metadata |
| `GET /policy/bundle?workspace=` | Policies with source, plus effective config |
| `PUT /policy/{name}` | Create or replace (compiles first; `400` on a Rego error, `409` if file-owned) |
| `DELETE /policy/{name}` | Remove |
| `GET`/`PUT /policy/config` | Scoped enforcement configuration |
| `GET /violations` | All current violation reports |
| `GET /violations/{workspace}` | One workspace; `404` when clean |

### Environment

| Variable | Effect |
|---|---|
| `TERRARIUM_POLICY_MODE` | `enforce`, `warn` or `off` for this invocation |
| `TERRARIUM_POLICY_TIMEOUT_MS` | Per-policy evaluation ceiling (default 2000) |

Evaluation is time-bounded because policies arrive from uploads and cloned repositories. A rule that runs long is aborted and reported as an error rather than being allowed to stall a push or hang an apply. Policies cannot reach the network, the filesystem, or other processes.

### Metrics

```text
terrarium_policy_evaluations_total{workspace,site,result}
terrarium_policy_violations_total{workspace,policy,severity}
terrarium_policy_evaluation_duration_seconds{site}
terrarium_policy_lint_skipped_total{workspace,reason}
terrarium_policies_loaded{origin}
```

Violation *messages* are never used as label values — see [observability.md](observability.md) for the cardinality rules these follow.
