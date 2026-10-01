#!/usr/bin/env bash
# Issue NAME.crt and NAME.key for localhost from the CA that sh/demo/ca.sh
# generates in the current directory. Chrome requires a <=14 day validity for
# self-signed certs.
#
# Usage: sh/demo/cert.sh NAME, from demo/relay.
set -euo pipefail
umask 077

name=${1:?usage: sh/demo/cert.sh NAME}

export OPENSSL_CONF="ca.cnf"
openssl req -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes \
    -subj "/CN=$name" \
    -keyout "$name.key" -out "$name.csr"
openssl x509 -req -sha256 -in "$name.csr" \
    -CA ca.pem -CAkey ca.key -CAcreateserial \
    -days 14 -extfile <(printf "subjectAltName=DNS:localhost\n") \
    -out "$name.crt"
rm "$name.csr"
