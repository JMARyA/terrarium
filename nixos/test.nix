# Called as `import ./nixos/test.nix { inherit pkgs nixosModule terrarium; }`
{ pkgs, nixosModule, terrarium }:
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

      environment.systemPackages = [ pkgs.curl pkgs.opentofu ];

      # Minimal OpenTofu root module for the compatibility test.
      # No providers or resources so tofu never tries to reach the internet.
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

      # Seed an admin user before terrarium starts so the service can
      # authenticate requests from the very first test step.
      system.activationScripts.terrariumSeedUsers = lib.stringAfter [ "users" "groups" ] ''
        mkdir -p /var/lib/terrarium
        TERRARIUM_DATA=/var/lib/terrarium ${terrarium}/bin/terra user add admin secret 2>/dev/null || true
        chown -R terrarium:terrarium /var/lib/terrarium
      '';
    };

  testScript = ''
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
  '';
}
