# Called as `import ./nixos/test.nix { inherit pkgs nixosModule terrarium dockerImage; }`
{ pkgs, nixosModule, terrarium, dockerImage }:
pkgs.testers.runNixOSTest {
  name = "terrarium";

  nodes.server =
    { config, lib, pkgs, ... }:
    {
      imports = [ nixosModule ];

      services.terrarium = {
        enable = true;
        package = terrarium;
      };

      environment.systemPackages = [ pkgs.curl pkgs.opentofu pkgs.zip ];

      # Terraform config for the state-backend compatibility test.
      environment.etc."terrarium-tofu-test/main.tf".text = ''
        terraform {
          backend "http" {
            address        = "http://localhost:8080/state/tofu-compat"
            lock_address   = "http://localhost:8080/lock/tofu-compat"
            unlock_address = "http://localhost:8080/lock/tofu-compat"
            username       = "admin"
            password       = "secret"
          }
        }

        output "hello" {
          value = "world"
        }
      '';

      # CLI config that installs from a local filesystem mirror.
      # OpenTofu requires HTTPS for network mirrors, so we download the provider
      # from terrarium via curl and hand it to tofu as a filesystem mirror instead.
      environment.etc."terrarium-registry-test/tofurc".text = ''
        provider_installation {
          filesystem_mirror {
            path    = "/tmp/fsmirror"
            include = ["example.com/test/myprovider"]
          }
          direct {
            exclude = ["example.com/test/myprovider"]
          }
        }
      '';

      # Provider config that sources from the fake example.com registry.
      environment.etc."terrarium-registry-test/main.tf".text = ''
        terraform {
          required_providers {
            myprovider = {
              source  = "example.com/test/myprovider"
              version = "1.0.0"
            }
          }
        }
      '';

      # Seed an admin user before terrarium starts so the service can
      # authenticate requests from the very first test step.
      system.activationScripts.terrariumSeedUsers = lib.stringAfter [ "users" "groups" ] ''
        mkdir -p /var/lib/terrarium
        TERRARIUM_DATA=/var/lib/terrarium ${terrarium}/bin/terra user add admin secret 2>/dev/null || true
        chown -R terrarium:terrarium /var/lib/terrarium
      '';
    };

  # Second node tests the published container image. Runs the same binary but
  # through Docker so we validate the image build (CA certs, env wiring, etc.).
  nodes.container =
    { config, pkgs, ... }:
    {
      virtualisation.docker.enable = true;
      virtualisation.diskSize = 4096;
      virtualisation.memorySize = 1024;
      environment.systemPackages = [ pkgs.docker pkgs.curl ];
    };

  testScript = let
    imageTag = "terrarium:latest-${pkgs.stdenv.hostPlatform.linuxArch}";
  in ''
    import json

    # ── NixOS module tests ──────────────────────────────────────────────────

    server.start()
    server.wait_for_unit("terrarium.service")
    server.wait_for_open_port(8080)

    # Unauthenticated access must be rejected
    server.fail("curl -sf http://localhost:8080/state")

    # Empty state list
    out = server.succeed("curl -sf -u admin:secret http://localhost:8080/state")
    assert out.strip() == "[]", f"Expected [], got: {out}"

    # Write a state blob via POST
    server.succeed(
        "printf '%s' "
        "'{\"version\":4,\"terraform_version\":\"1.9.0\","
        "\"serial\":1,\"lineage\":\"test\",\"outputs\":{},\"resources\":[]}'"
        " > /tmp/state.json"
    )
    server.succeed(
        "curl -sf -u admin:secret -X POST http://localhost:8080/state/prod/web"
        " -H 'Content-Type: application/json' -d @/tmp/state.json"
    )

    # Retrieve the state
    server.succeed("curl -sf -u admin:secret http://localhost:8080/state/prod/web")

    # Must appear in the listing
    out = server.succeed("curl -sf -u admin:secret http://localhost:8080/state")
    assert "prod/web" in out, f"State missing from listing: {out}"

    # Version 1 must exist
    out = server.succeed(
        "curl -sf -u admin:secret http://localhost:8080/versions/prod/web"
    )
    assert "1" in out, f"Version 1 missing: {out}"

    # Push a second revision
    server.succeed(
        "printf '%s' "
        "'{\"version\":4,\"terraform_version\":\"1.9.0\","
        "\"serial\":2,\"lineage\":\"test\",\"outputs\":{},\"resources\":[]}'"
        " > /tmp/state2.json"
    )
    server.succeed(
        "curl -sf -u admin:secret -X POST http://localhost:8080/state/prod/web"
        " -H 'Content-Type: application/json' -d @/tmp/state2.json"
    )
    out = server.succeed(
        "curl -sf -u admin:secret http://localhost:8080/versions/prod/web"
    )
    assert "2" in out, f"Version 2 missing: {out}"

    # Acquire a lock
    server.succeed(
        "printf '%s' "
        "'{\"ID\":\"lock-1\",\"Operation\":\"OperationTypeApply\","
        "\"Info\":\"\",\"Who\":\"admin\",\"Version\":\"1.9.0\","
        "\"Created\":\"2024-01-01T00:00:00.000Z\",\"Path\":\"prod/web\"}'"
        " > /tmp/lock.json"
    )
    server.succeed(
        "curl -sf -u admin:secret -X POST http://localhost:8080/lock/prod/web"
        " -H 'Content-Type: application/json' -d @/tmp/lock.json"
    )

    # Lock must appear in the active lock listing
    out = server.succeed("curl -sf -u admin:secret http://localhost:8080/lock")
    assert "prod/web" in out, f"Lock missing from listing: {out}"

    # Acquiring a second lock must fail (409 Conflict)
    server.succeed(
        "printf '%s' "
        "'{\"ID\":\"lock-2\",\"Operation\":\"OperationTypeApply\","
        "\"Info\":\"\",\"Who\":\"admin\",\"Version\":\"1.9.0\","
        "\"Created\":\"2024-01-01T00:00:01.000Z\",\"Path\":\"prod/web\"}'"
        " > /tmp/lock2.json"
    )
    server.fail(
        "curl -sf -u admin:secret -X POST http://localhost:8080/lock/prod/web"
        " -H 'Content-Type: application/json' -d @/tmp/lock2.json"
    )

    # Unlock
    server.succeed(
        "curl -sf -u admin:secret -X DELETE http://localhost:8080/lock/prod/web"
    )
    out = server.succeed("curl -sf -u admin:secret http://localhost:8080/lock")
    assert "prod/web" not in out, f"Lock still present after release: {out}"

    # Archive the state
    server.succeed(
        "curl -sf -u admin:secret -X POST http://localhost:8080/archive/prod/web"
    )

    # Archived state must be absent from the default listing
    out = server.succeed("curl -sf -u admin:secret http://localhost:8080/state")
    assert "prod/web" not in out, f"Archived state should not appear in default list: {out}"

    # Archived state must appear when ?archived=true
    out = server.succeed(
        "curl -sf -u admin:secret 'http://localhost:8080/state?archived=true'"
    )
    assert "prod/web" in out, f"Archived state missing from archived list: {out}"

    # Writes to an archived state must be rejected (403)
    server.fail(
        "curl -sf -u admin:secret -X POST http://localhost:8080/state/prod/web"
        " -H 'Content-Type: application/json' -d @/tmp/state.json"
    )

    # Unarchive
    server.succeed(
        "curl -sf -u admin:secret -X DELETE http://localhost:8080/archive/prod/web"
    )
    out = server.succeed("curl -sf -u admin:secret http://localhost:8080/state")
    assert "prod/web" in out, f"Unarchived state missing from listing: {out}"

    # Delete the state
    server.succeed(
        "curl -sf -u admin:secret -X DELETE http://localhost:8080/state/prod/web"
    )
    out = server.succeed("curl -sf -u admin:secret http://localhost:8080/state")
    assert "prod/web" not in out, f"Deleted state still appears in listing: {out}"

    # ── OpenTofu end-to-end compatibility ──────────────────────────────────

    # Copy the pre-built config into a writable directory; tofu writes
    # .terraform/ and .terraform.lock.hcl there during init.
    server.succeed("cp -rT /etc/terrarium-tofu-test /tmp/tf")

    # init — connects to the HTTP backend, no provider downloads needed
    server.succeed("cd /tmp/tf && tofu init -input=false -no-color")

    # apply — exercises the full lock → GET state → POST state → unlock cycle
    server.succeed("cd /tmp/tf && tofu apply -auto-approve -input=false -no-color")

    # terrarium must have stored the state
    out = server.succeed(
        "curl -sf -u admin:secret http://localhost:8080/state/tofu-compat"
    )
    assert "serial" in out, f"tofu-compat state not stored: {out}"

    # version 1 must exist
    out = server.succeed(
        "curl -sf -u admin:secret http://localhost:8080/versions/tofu-compat"
    )
    assert "1" in out, f"tofu-compat version 1 missing: {out}"

    # no lock must be dangling after a clean apply
    out = server.succeed("curl -sf -u admin:secret http://localhost:8080/lock")
    assert "tofu-compat" not in out, f"Lock not released after tofu apply: {out}"

    # second apply — confirms the lock/unlock cycle can be repeated cleanly
    server.succeed("cd /tmp/tf && tofu apply -auto-approve -input=false -no-color")
    out = server.succeed("curl -sf -u admin:secret http://localhost:8080/lock")
    assert "tofu-compat" not in out, f"Lock not released after second tofu apply: {out}"

    # ── Provider registry ───────────────────────────────────────────────────

    # Build a minimal fake provider zip.  tofu init downloads and hash-checks
    # the archive but does not execute the binary, so a shell script is fine.
    server.succeed("mkdir -p /tmp/prov")
    server.succeed(
        "printf '#!/bin/sh\necho fake-provider' "
        "> /tmp/prov/terraform-provider-myprovider_v1.0.0_x5"
    )
    server.succeed("chmod +x /tmp/prov/terraform-provider-myprovider_v1.0.0_x5")
    server.succeed(
        "cd /tmp/prov && zip "
        "terraform-provider-myprovider_1.0.0_linux_amd64.zip "
        "terraform-provider-myprovider_v1.0.0_x5"
    )

    # Upload the provider to the terrarium registry
    server.succeed(
        "curl -sf -u admin:secret -X POST "
        "http://localhost:8080/registry/providers/test/myprovider/1.0.0/linux/amd64 "
        "--data-binary @/tmp/prov/terraform-provider-myprovider_1.0.0_linux_amd64.zip"
    )

    # Upload docs for the provider
    server.succeed(
        "curl -sf -u admin:secret -X PUT "
        "http://localhost:8080/registry/providers/test/myprovider/1.0.0/docs "
        "-d '# My Provider\n\nA test provider for terrarium.'"
    )

    # Service discovery endpoint
    out = server.succeed("curl -sf http://localhost:8080/.well-known/terraform.json")
    assert "providers.v1" in out, f"service discovery missing providers.v1: {out}"

    # Provider registry v1 — list versions
    out = server.succeed(
        "curl -sf http://localhost:8080/registry/v1/providers/test/myprovider/versions"
    )
    assert "1.0.0" in out, f"version 1.0.0 missing from registry: {out}"
    assert "linux" in out, f"platform missing from registry: {out}"

    # Provider registry v1 — download info
    out = server.succeed(
        "curl -sf http://localhost:8080/registry/v1/providers/test/myprovider/1.0.0/download/linux/amd64"
    )
    assert "download_url" in out, f"download_url missing: {out}"
    assert "shasum" in out, f"shasum missing: {out}"

    # Download the binary back and verify it matches the upload
    server.succeed(
        "curl -sf -u admin:secret "
        "http://localhost:8080/registry/providers/test/myprovider/1.0.0/linux/amd64/zip "
        "> /tmp/prov/downloaded.zip"
    )
    orig = server.succeed("sha256sum /tmp/prov/terraform-provider-myprovider_1.0.0_linux_amd64.zip").split()[0]
    got  = server.succeed("sha256sum /tmp/prov/downloaded.zip").split()[0]
    assert orig == got, f"Downloaded zip hash mismatch: {orig} vs {got}"

    # Network mirror protocol — index
    out = server.succeed(
        "curl -sf "
        "http://localhost:8080/registry/mirror/example.com/test/myprovider/index.json"
    )
    assert "1.0.0" in out, f"version missing from mirror index: {out}"

    # Network mirror protocol — version archives
    out = server.succeed(
        "curl -sf "
        "http://localhost:8080/registry/mirror/example.com/test/myprovider/1.0.0.json"
    )
    assert "archives" in out, f"archives missing from mirror version: {out}"
    assert "zh:" in out, f"zh: hash missing from mirror: {out}"
    assert "linux_amd64" in out, f"platform missing from mirror: {out}"

    # OpenTofu end-to-end: download the provider from terrarium and hand it to
    # tofu via a filesystem mirror (OpenTofu requires HTTPS for network mirrors,
    # so we exercise the download endpoint directly and let tofu verify the zip).
    server.succeed(
        "mkdir -p '/tmp/fsmirror/example.com/test/myprovider' /tmp/reg-tofu"
    )
    server.succeed(
        "curl -sf -u admin:secret "
        "http://localhost:8080/registry/providers/test/myprovider/1.0.0/linux/amd64/zip "
        "-o '/tmp/fsmirror/example.com/test/myprovider/terraform-provider-myprovider_1.0.0_linux_amd64.zip'"
    )
    server.succeed("cp /etc/terrarium-registry-test/main.tf /tmp/reg-tofu/")
    server.succeed(
        "cd /tmp/reg-tofu && TF_CLI_CONFIG_FILE=/etc/terrarium-registry-test/tofurc "
        "tofu init -input=false -no-color"
    )

    # Provider must be extracted into the tofu plugin cache after init
    server.succeed(
        "find /tmp/reg-tofu/.terraform -name 'terraform-provider-myprovider*' | grep -q ."
    )

    # Web UI — registry index page renders provider list
    out = server.succeed("curl -sf http://localhost:8080/registry")
    assert "myprovider" in out, f"myprovider missing from registry UI: {out}"

    # Web UI — provider detail page redirects to docs; -L follows the redirect
    out = server.succeed("curl -sfL http://localhost:8080/registry/test/myprovider")
    assert "1.0.0" in out, f"version missing from provider detail page: {out}"
    assert "My Provider" in out, f"docs missing from provider detail page: {out}"

    # Mirror status endpoint — no mirrors configured, should return idle status
    out = server.succeed("curl -sf http://localhost:8080/registry/status")
    status = json.loads(out)
    assert "running" in status, f"/registry/status missing 'running' field: {out}"
    assert status["running"] == False, f"/registry/status should be idle: {out}"

    # ── Container image tests ───────────────────────────────────────────────
    # These run in a separate VM that loads the Docker image, exercising the
    # container path that most production deployments use.

    container.start()
    container.wait_for_unit("docker.service")
    container.succeed("docker load < ${dockerImage}")

    # Image must declare SSL_CERT_FILE and SSL_CERT_DIR so OpenSSL can reach
    # the CA bundle inside the scratch container (the primary bug fix).
    env_json = container.succeed(
        "docker inspect ${imageTag} --format '{{json .Config.Env}}'"
    )
    envs = json.loads(env_json)
    assert any("SSL_CERT_FILE" in e for e in envs), \
        f"SSL_CERT_FILE missing from image config (CA cert fix): {envs}"
    assert any("SSL_CERT_DIR" in e for e in envs), \
        f"SSL_CERT_DIR missing from image config: {envs}"

    # Start the container
    container.succeed("mkdir -p /tmp/terra-data")
    container.succeed(
        "docker run -d -p 8080:8080 --name terra "
        "-e TERRARIUM_DATA=/app -e RUST_LOG=info "
        "-v /tmp/terra-data:/app "
        "${imageTag}"
    )
    container.wait_until_succeeds(
        "curl -sf http://localhost:8080/.well-known/terraform.json",
        timeout=30,
    )

    # CA cert bundle must be present on disk inside the running container.
    # The image has no shell or coreutils, so copy it out and test on the host.
    container.succeed("docker cp terra:/etc/ssl/certs/ca-bundle.crt /tmp/ca-bundle.crt")
    container.succeed("test -s /tmp/ca-bundle.crt")

    # Add an admin user and run basic state-backend assertions against the
    # container — same happy-path as the module node above.
    container.succeed("docker exec terra /bin/terra user add admin secret")

    out = container.succeed("curl -sf -u admin:secret http://localhost:8080/state")
    assert out.strip() == "[]", f"container: expected empty state list, got: {out}"

    container.fail("curl -sf http://localhost:8080/state")

    container.succeed(
        "printf '%s' "
        "'{\"version\":4,\"terraform_version\":\"1.9.0\","
        "\"serial\":1,\"lineage\":\"test\",\"outputs\":{},\"resources\":[]}'"
        " > /tmp/ctr-state.json"
    )
    container.succeed(
        "curl -sf -u admin:secret -X POST http://localhost:8080/state/test/state"
        " -H 'Content-Type: application/json' -d @/tmp/ctr-state.json"
    )
    out = container.succeed("curl -sf -u admin:secret http://localhost:8080/state")
    assert "test/state" in out, f"container: state not in listing: {out}"

    # Mirror status endpoint — idle since no mirrors.json in /tmp/terra-data
    out = container.succeed("curl -sf http://localhost:8080/registry/status")
    status = json.loads(out)
    assert "running" in status, f"container: /registry/status missing 'running': {out}"
    assert status["running"] == False, f"container: /registry/status should be idle: {out}"
  '';
}
