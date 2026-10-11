//! `/help` — public how-to. Snippets use this server's real address, taken
//! from the request.

use authur::MaybeUser;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use maud::{Markup, html};

use super::components::{codeblock, icon};
use super::layout::{Section, Shell, render};
use super::origin;
use crate::AppState;

fn step(id: &str, ic: &str, title: &str, body: Markup) -> Markup {
    html! {
        section.card id=(id) {
            div.card-h { h2 { (title) } span.aside { (icon(ic)) } }
            div.card-b.prose style="max-width:none" { (body) }
        }
    }
}

pub async fn page(maybe: MaybeUser, State(app): State<AppState>, headers: HeaderMap) -> Response {
    let user = maybe.user().map(|u| u.username.clone());
    let shell = Shell::new(&app, user.as_deref(), Section::Help);
    let url = origin(&headers);
    let host = url.trim_start_matches("https://").trim_start_matches("http://");
    let who = user.as_deref().unwrap_or("alice");

    let body = html! {
        div.head {
            div.head-text {
                h1 { "Using terrarium" }
                p.lede { "Everything below is ready to paste — the address is this server's." }
            }
        }
        div.help-steps {
            (step("backend", "layers", "Keep state here", html! {
                p { "Point a Terraform or OpenTofu " code { "http" } " backend at a workspace. It's created on first push; names can nest (" code { "infra/prod" } ")." }
                (codeblock(&format!(r#"terraform {{
  backend "http" {{
    address        = "{url}/state/infra/prod"
    lock_address   = "{url}/lock/infra/prod"
    unlock_address = "{url}/lock/infra/prod"
  }}
}}"#)))
                p { "Then authenticate and init — an " a href="/tokens" { "API token" } " is better than your password:" }
                (codeblock(&format!("export TF_HTTP_USERNAME={who}\nexport TF_HTTP_PASSWORD=<password-or-api-token>\nterra init")))
            }))
            (step("cli", "arrow", "The terra CLI", html! {
                p { code { "terra" } " passes every OpenTofu command straight through, and adds a few of its own." }
                (codeblock(&format!(r#"terra init / plan / apply / destroy …   # same as tofu, policies checked before apply

terra terra-login                       # save {url} + credentials
terra remote state list                 # all workspaces
terra remote state versions infra/prod  # version history
terra remote state diff infra/prod 3 4  # structural diff
terra remote state unlock infra/prod    # force-unlock
terra remote lock list                  # who holds what
terra remote webhook add infra/prod https://hooks.example/tf"#)))
            }))
            (step("registry", "registry", "Publish providers", html! {
                p { "Push a provider build, one platform at a time:" }
                (codeblock(&format!(r#"curl -u {who}:<token> -X POST \
  "{url}/registry/providers/<namespace>/<type>/<version>/<os>/<arch>" \
  --data-binary @terraform-provider-<type>_<version>_<os>_<arch>.zip"#)))
                p { "Upload docs from " a href="https://github.com/hashicorp/terraform-plugin-docs" { "terraform-plugin-docs" } " — they render on the " a href="/registry" { "registry page" } ":" }
                (codeblock(&format!(r#"zip -r docs.zip docs/
curl -u {who}:<token> -X PUT \
  "{url}/registry/providers/<namespace>/<type>/<version>/docs" \
  --data-binary @docs.zip"#)))
                p { "And use it:" }
                (codeblock(&format!(r#"terraform {{
  required_providers {{
    example = {{
      source  = "{host}/<namespace>/<type>"
      version = "<version>"
    }}
  }}
}}"#)))
            }))
            (step("mirror", "layers", "Mirror upstream providers", html! {
                p { "Use terrarium as a network mirror — providers are fetched from upstream on first use, then served locally. Needs HTTPS." }
                (codeblock(&format!(r#"# ~/.tofurc
provider_installation {{
  network_mirror {{
    url     = "{url}/registry/mirror/"
    include = ["registry.opentofu.org/*/*", "registry.terraform.io/*/*"]
  }}
  direct {{
    exclude = ["registry.opentofu.org/*/*", "registry.terraform.io/*/*"]
  }}
}}"#)))
                p { "Optionally pre-warm, via the API or a " code { "mirrors.json" } " in the data directory (refreshed every " code { "TERRARIUM_MIRROR_INTERVAL" } " seconds):" }
                (codeblock(&format!(r#"curl -u {who}:<token> -X POST "{url}/registry/mirror" \
  -H "Content-Type: application/json" \
  -d '{{"namespace": "hashicorp", "type": "aws", "versions": ["5.60.0"]}}'"#)))
            }))
            (step("auth", "key", "Authentication", html! {
                p { "Every API endpoint takes either:" }
                ul {
                    li { b { "HTTP Basic" } " — username and password, or username and an API token. What the Terraform backend uses." }
                    li { b { "Bearer token" } " — " code { "Authorization: Bearer <token>" } ", minted on the " a href="/tokens" { "Tokens" } " page." }
                }
                p { "Read-only registry endpoints (version lists, downloads, the mirror) are public." }
            }))
            (step("env", "overview", "Configuration", html! {
                (codeblock(r#"TERRARIUM_DATA              data directory (default: current dir)
TERRARIUM_TLS               1 = mark session cookies Secure (when behind TLS)
TERRARIUM_LISTEN            address to listen on (default 0.0.0.0:8080)
TERRARIUM_LOCK_TTL          seconds before a lock may be taken over (default 7200, 0 = never)
TERRARIUM_MAX_BODY_BYTES    request body cap (default 256 MiB)
TERRARIUM_WEBHOOK_NETWORKS  where webhooks may deliver
TERRARIUM_MIRROR_INTERVAL   re-sync mirrors.json every N seconds
TERRARIUM_UPSTREAM_REGISTRIES  upstreams allowed for the lazy mirror
TERRARIUM_METRICS           1 = expose /metrics (with TERRARIUM_METRICS_TOKEN_FILE)

TERRARIUM_URL / TERRARIUM_USER / TERRARIUM_PASSWORD / TERRARIUM_CONFIG   client side"#))
            }))
        }
        p.note style="margin-top:22px" { "Tip: press " kbd { "/" } " or " kbd { "Ctrl K" } " anywhere to jump to a workspace. " code { "infra/prod@3" } " opens version 3." }
    };
    render(&shell, "Help", body).into_response()
}
