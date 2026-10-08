# [XS] Refuse ignored QUIC load-balancer settings

## Goal

`--listen-quic-lb-nonce` without `--listen-quic-lb-id`, set through TOML or
env, stops startup with an error instead of being ignored, and `lb_id` no
longer silently overrides `load_balancer`: setting both is refused. Follows
#5006, which refused the other ignored listener settings.
