# State Migration Guide

The CDN tofu configuration has been split from a single root module into four
independent root modules (`common/`, `relay/`, `pub/`, `boy/`). If you have an
existing deployment with state in `cdn/tofu.tfstate`, follow this guide to
migrate your state without recreating infrastructure.

## Prerequisites

- Back up your existing state file before starting:
  ```bash
  cp cdn/tofu.tfstate cdn/tofu.tfstate.pre-migration
  ```
- Create `terraform.tfvars` in each new module directory (see `terraform.tfvars.example`)

## Phase 1: Initialize new modules

```bash
just cdn init
```

## Phase 2: Move shared resources to `common/`

```bash
cd cdn

# GCP project services
tofu state mv -state=tofu.tfstate -state-out=common/tofu.tfstate \
    'google_project_service.all["dns.googleapis.com"]' \
    'google_project_service.all["dns.googleapis.com"]'
tofu state mv -state=tofu.tfstate -state-out=common/tofu.tfstate \
    'google_project_service.all["monitoring.googleapis.com"]' \
    'google_project_service.all["monitoring.googleapis.com"]'

# Service account (renamed from relay -> cdn in terraform, but GCP account_id stays)
tofu state mv -state=tofu.tfstate -state-out=common/tofu.tfstate \
    google_service_account.relay google_service_account.cdn
tofu state mv -state=tofu.tfstate -state-out=common/tofu.tfstate \
    google_service_account_key.relay google_service_account_key.cdn

# Bootstrap stackscript
tofu state mv -state=tofu.tfstate -state-out=common/tofu.tfstate \
    linode_stackscript.bootstrap linode_stackscript.bootstrap

# DNS zone (renamed from relay -> cdn in terraform, GCP name stays "relay-cdn")
tofu state mv -state=tofu.tfstate -state-out=common/tofu.tfstate \
    google_dns_managed_zone.relay google_dns_managed_zone.cdn

# IAM binding
tofu state mv -state=tofu.tfstate -state-out=common/tofu.tfstate \
    google_project_iam_member.dns_admin google_project_iam_member.dns_admin

# Monitor service file
tofu state mv -state=tofu.tfstate -state-out=common/tofu.tfstate \
    local_file.monitor_service local_file.monitor_service
```

## Phase 3: Move relay resources

Resources are moved from `module.relay.*` to top-level addresses (stripping the module prefix).

```bash
cd cdn

tofu state mv -state=tofu.tfstate -state-out=relay/tofu.tfstate \
    'module.relay.linode_instance.relay["usc"]' 'linode_instance.relay["usc"]'
tofu state mv -state=tofu.tfstate -state-out=relay/tofu.tfstate \
    'module.relay.linode_instance.relay["usw"]' 'linode_instance.relay["usw"]'
tofu state mv -state=tofu.tfstate -state-out=relay/tofu.tfstate \
    'module.relay.linode_instance.relay["use"]' 'linode_instance.relay["use"]'
tofu state mv -state=tofu.tfstate -state-out=relay/tofu.tfstate \
    'module.relay.linode_instance.relay["euc"]' 'linode_instance.relay["euc"]'
tofu state mv -state=tofu.tfstate -state-out=relay/tofu.tfstate \
    'module.relay.linode_instance.relay["sea"]' 'linode_instance.relay["sea"]'

tofu state mv -state=tofu.tfstate -state-out=relay/tofu.tfstate \
    'module.relay.random_password.relay_root["usc"]' 'random_password.relay_root["usc"]'
tofu state mv -state=tofu.tfstate -state-out=relay/tofu.tfstate \
    'module.relay.random_password.relay_root["usw"]' 'random_password.relay_root["usw"]'
tofu state mv -state=tofu.tfstate -state-out=relay/tofu.tfstate \
    'module.relay.random_password.relay_root["use"]' 'random_password.relay_root["use"]'
tofu state mv -state=tofu.tfstate -state-out=relay/tofu.tfstate \
    'module.relay.random_password.relay_root["euc"]' 'random_password.relay_root["euc"]'
tofu state mv -state=tofu.tfstate -state-out=relay/tofu.tfstate \
    'module.relay.random_password.relay_root["sea"]' 'random_password.relay_root["sea"]'

tofu state mv -state=tofu.tfstate -state-out=relay/tofu.tfstate \
    module.relay.linode_firewall.relay linode_firewall.relay

tofu state mv -state=tofu.tfstate -state-out=relay/tofu.tfstate \
    'module.relay.google_dns_record_set.relay_node["usc"]' 'google_dns_record_set.relay_node["usc"]'
tofu state mv -state=tofu.tfstate -state-out=relay/tofu.tfstate \
    'module.relay.google_dns_record_set.relay_node["usw"]' 'google_dns_record_set.relay_node["usw"]'
tofu state mv -state=tofu.tfstate -state-out=relay/tofu.tfstate \
    'module.relay.google_dns_record_set.relay_node["use"]' 'google_dns_record_set.relay_node["use"]'
tofu state mv -state=tofu.tfstate -state-out=relay/tofu.tfstate \
    'module.relay.google_dns_record_set.relay_node["euc"]' 'google_dns_record_set.relay_node["euc"]'
tofu state mv -state=tofu.tfstate -state-out=relay/tofu.tfstate \
    'module.relay.google_dns_record_set.relay_node["sea"]' 'google_dns_record_set.relay_node["sea"]'

tofu state mv -state=tofu.tfstate -state-out=relay/tofu.tfstate \
    module.relay.google_dns_record_set.relay_global google_dns_record_set.relay_global

tofu state mv -state=tofu.tfstate -state-out=relay/tofu.tfstate \
    module.relay.local_file.moq_relay_service local_file.moq_relay_service
tofu state mv -state=tofu.tfstate -state-out=relay/tofu.tfstate \
    module.relay.local_file.moq_cert_service local_file.moq_cert_service
```

## Phase 4: Move pub resources

```bash
cd cdn

tofu state mv -state=tofu.tfstate -state-out=pub/tofu.tfstate \
    module.pub.linode_instance.publisher linode_instance.publisher
tofu state mv -state=tofu.tfstate -state-out=pub/tofu.tfstate \
    module.pub.random_password.publisher_root random_password.publisher_root
tofu state mv -state=tofu.tfstate -state-out=pub/tofu.tfstate \
    module.pub.linode_firewall.publisher linode_firewall.publisher
tofu state mv -state=tofu.tfstate -state-out=pub/tofu.tfstate \
    module.pub.local_file.demo_bbb_service local_file.demo_bbb_service

# Move the pub DNS record (this was in the root, not inside the module)
tofu state mv -state=tofu.tfstate -state-out=pub/tofu.tfstate \
    google_dns_record_set.publisher google_dns_record_set.publisher
```

## Phase 5: Move boy resources

```bash
cd cdn

tofu state mv -state=tofu.tfstate -state-out=boy/tofu.tfstate \
    module.boy.linode_instance.boy linode_instance.boy
tofu state mv -state=tofu.tfstate -state-out=boy/tofu.tfstate \
    module.boy.random_password.boy_root random_password.boy_root
tofu state mv -state=tofu.tfstate -state-out=boy/tofu.tfstate \
    module.boy.linode_firewall.boy linode_firewall.boy
tofu state mv -state=tofu.tfstate -state-out=boy/tofu.tfstate \
    module.boy.local_file.boy_prepare_service local_file.boy_prepare_service

tofu state mv -state=tofu.tfstate -state-out=boy/tofu.tfstate \
    'module.boy.local_file.boy_service["big2small"]' 'local_file.boy_service["big2small"]'
tofu state mv -state=tofu.tfstate -state-out=boy/tofu.tfstate \
    'module.boy.local_file.boy_service["dangan"]' 'local_file.boy_service["dangan"]'
tofu state mv -state=tofu.tfstate -state-out=boy/tofu.tfstate \
    'module.boy.local_file.boy_service["opossum"]' 'local_file.boy_service["opossum"]'
tofu state mv -state=tofu.tfstate -state-out=boy/tofu.tfstate \
    'module.boy.local_file.boy_service["capybara"]' 'local_file.boy_service["capybara"]'
tofu state mv -state=tofu.tfstate -state-out=boy/tofu.tfstate \
    'module.boy.local_file.boy_service["fofk"]' 'local_file.boy_service["fofk"]'
tofu state mv -state=tofu.tfstate -state-out=boy/tofu.tfstate \
    'module.boy.local_file.boy_service["gb-run"]' 'local_file.boy_service["gb-run"]'

# Move the boy DNS record (this was in the root, not inside the module)
tofu state mv -state=tofu.tfstate -state-out=boy/tofu.tfstate \
    google_dns_record_set.boy google_dns_record_set.boy
```

## Phase 6: Verify

The old state should now be empty:
```bash
cd cdn
tofu state list  # should output nothing
```

Verify each module shows no planned changes:
```bash
(cd cdn/common && tofu plan)
(cd cdn/relay && tofu plan)
(cd cdn/pub && tofu plan)
(cd cdn/boy && tofu plan)
```

If any module shows planned changes, review carefully. Data sources
(`terraform_remote_state`) will appear as "read" operations which is expected
and safe.

## Phase 7: Clean up

Once verified, remove the old state file:
```bash
rm cdn/tofu.tfstate cdn/tofu.tfstate.backup
```
