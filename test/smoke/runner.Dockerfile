# The source-built relay follows the runner's /certs convention. Its scratch
# image needs a shell and grep for Docker Compose's UDP healthcheck.
FROM moq-interop-relay-base:local
COPY --from=busybox:stable /bin /bin
ENV MOQ_LISTEN=[::]:4443 \
    MOQ_LISTEN_TLS_CERT=/certs/cert.pem \
    MOQ_LISTEN_TLS_KEY=/certs/priv.key \
    MOQ_AUTH_PUBLIC=**
EXPOSE 4443/udp
