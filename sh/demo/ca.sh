#!/usr/bin/env bash
# Generate the relay cluster's self-signed CA (ca.pem, ca.key, ca.cnf) in the
# current directory, unless it already exists.
#
# Usage: sh/demo/ca.sh, from demo/relay.
set -euo pipefail
umask 077

[ -f ca.pem ] && [ -f ca.key ] && exit 0
rm -f ca.pem ca.key
printf '[req]\ndistinguished_name = req_dn\n[req_dn]\n' >ca.cnf
export OPENSSL_CONF="ca.cnf"
openssl req -x509 -sha256 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes \
    -days 365 -subj "/CN=moq cluster CA" \
    -keyout ca.key -out ca.pem
