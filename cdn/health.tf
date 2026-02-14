# Health check configuration
variable "health_email" {
  description = "Email address for health check failure notifications"
  type        = string
  default     = ""
}

# Read the JWT from the secrets directory, same as other tokens
locals {
  health_jwt = trimspace(file("${path.module}/secrets/demo-sub.jwt"))
}

# HTTPS uptime check for each relay node
resource "google_monitoring_uptime_check_config" "relay" {
  for_each = local.relays

  display_name = "relay-${each.key}"
  timeout      = "10s"
  period       = "300s" # every 5 minutes

  http_check {
    path         = "/fetch/demo/bbb/catalog.json?jwt=${local.health_jwt}"
    port         = 443
    use_ssl      = true
    validate_ssl = true
  }

  monitored_resource {
    type = "uptime_url"

    labels = {
      project_id = var.gcp_project
      host       = "${each.key}.${var.domain}"
    }
  }
}

# Email notification channel (created only if email is provided)
resource "google_monitoring_notification_channel" "email" {
  count = var.health_email != "" ? 1 : 0

  display_name = "MoQ CDN Health Alerts"
  type         = "email"

  labels = {
    email_address = var.health_email
  }
}

# Alert policy that fires when any node health check fails
resource "google_monitoring_alert_policy" "relay_down" {
  count = var.health_email != "" ? 1 : 0

  display_name = "MoQ Relay Node Down"
  combiner     = "OR"

  conditions {
    display_name = "Uptime check failing"

    condition_threshold {
      filter          = "resource.type = \"uptime_url\" AND metric.type = \"monitoring.googleapis.com/uptime_check/check_passed\""
      duration        = "300s" # must fail for 5 minutes before alerting
      comparison      = "COMPARISON_GT"
      threshold_value = 1

      aggregations {
        alignment_period     = "300s"
        per_series_aligner   = "ALIGN_NEXT_OLDER"
        cross_series_reducer = "REDUCE_COUNT_FALSE"
        group_by_fields      = ["resource.label.host"]
      }

      trigger {
        count = 1
      }
    }
  }

  notification_channels = [google_monitoring_notification_channel.email[0].name]

  alert_strategy {
    auto_close = "1800s" # auto-resolve after 30 minutes of recovery
  }
}
