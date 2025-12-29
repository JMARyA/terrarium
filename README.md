# 🌱 Terrarium

> **A safe enclosure for your Terraform state.** 🦎🪴

**Terrarium** is a small, boring, correct **Terraform HTTP state backend**.

It stores Terraform state as an **opaque blob**, provides **strict locking**, and stays completely out of your way.

No S3.
No Terraform Cloud.
No vendor assumptions.

## Why does this exist?

Terraform state is:

* critical
* shared
* easy to corrupt

Terrarium exists because:

* storing state in Git is unsafe
* S3 should not be mandatory
* the Terraform HTTP backend deserves a first-class server

## Features

* 🌱 Terraform-compatible HTTP backend
* 🔒 Strong, explicit state locking
* 🪴 Opaque state storage
* 🦎 Single static binary
* 🧱 Cloud-agnostic
* 🔐 Simple authentication

## Terraform configuration

```hcl
terraform {
  backend "http" {
    address        = "https://terrarium.example/state/prod"
    lock_address   = "https://terrarium.example/lock/prod"
    unlock_address = "https://terrarium.example/lock/prod"

    lock_method    = "POST"
    unlock_method  = "DELETE"
  }
}
```

You can provide auth credentials via the environment variables `$TF_HTTP_USERNAME` & `$TF_HTTP_PASSWORD`.
After that you need to reinit with `tofu init`. It will ask you to migrate any local state to the new backend.

## User Management
Only authenticated users can interact with the terraform state files. To create a user, you can use the CLI:

```shell
terrarium user add <username>
```
