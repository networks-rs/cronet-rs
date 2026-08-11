#!/usr/bin/env bash
set -euo pipefail

NGINX_ROOT=/opt/nginx-gmssl
CERT_ROOT="$NGINX_ROOT/gmssl-tests/tlcp-certs"
RUNTIME=/tmp/nginx-gmssl-e2e

mkdir -p "$RUNTIME/logs"
cp "$CERT_ROOT/signcert.pem" "$RUNTIME/tls-certs.pem"
cp "$CERT_ROOT/signkey.pem" "$RUNTIME/tls-signkey.pem"
cp "$CERT_ROOT/cacert.pem" "$RUNTIME/tls-ca.pem"
cp "$CERT_ROOT/rootcacert.pem" "$RUNTIME/tls-root.pem"
cp "$NGINX_ROOT/gmssl-tests/pass.txt" "$RUNTIME/pass.txt"
cp "$NGINX_ROOT/gmssl-tests/tlcp-certs/double_certs.pem" "$RUNTIME/double-certs.pem"
cp "$NGINX_ROOT/gmssl-tests/tlcp-certs/double_keys.pem" "$RUNTIME/double-keys.pem"
cp -R "$NGINX_ROOT/gmssl-tests/html" "$RUNTIME/html"
sed -n '/-----BEGIN CERTIFICATE-----/,$p' "$CERT_ROOT/cacert.pem" >>"$RUNTIME/tls-certs.pem"
sed -n '/-----BEGIN CERTIFICATE-----/,$p' "$CERT_ROOT/rootcacert.pem" >>"$RUNTIME/tls-certs.pem"

sed \
    -e "s|@RUNTIME@|$RUNTIME|g" \
    tests/gmssl-e2e/nginx.conf.in >"$RUNTIME/nginx.conf"

"$NGINX_ROOT/objs/nginx" -p "$RUNTIME" -c "$RUNTIME/nginx.conf" &
NGINX_PID=$!
cleanup() {
    status=$?
    kill "$NGINX_PID" 2>/dev/null || true
    wait "$NGINX_PID" 2>/dev/null || true
    if ((status != 0)) && [[ -f "$RUNTIME/logs/error.log" ]]; then
        sed -n '1,260p' "$RUNTIME/logs/error.log"
    fi
    exit "$status"
}
trap cleanup EXIT
sleep 1
kill -0 "$NGINX_PID"

GMSSL_E2E_CERTIFICATE_ROOT="$CERT_ROOT" \
GMSSL_E2E_CLIENT_CERTIFICATE="$RUNTIME/tls-certs.pem" \
GMSSL_E2E_CLIENT_PRIVATE_KEY="$RUNTIME/tls-signkey.pem" \
GMSSL_E2E_CLIENT_KEY_PASSWORD="$(tr -d '\r\n' <"$RUNTIME/pass.txt")" \
    cargo test -p tokio-cronet \
        --no-default-features \
        --features gmssl-tests \
        --test gmssl_nginx_e2e \
        -- --nocapture
