#!/usr/bin/env bash
# NatForge — full local end-to-end test.
# Brings up Postgres+Redis (docker), both planes, three origin services, and an
# agent with three routes, then verifies: HTTP-by-subdomain, HTTPS-by-SNI,
# raw TCP, two users sharing one port, multi-route over one session, and state
# surviving a both-planes restart. Run from the repo root:  bash scripts/e2e.sh
set -u
cd "$(dirname "$0")/.."
source "$HOME/.cargo/env" 2>/dev/null || true

PIDS=()
cleanup() { for p in "${PIDS[@]:-}"; do kill "$p" 2>/dev/null; done; wait 2>/dev/null; }
trap cleanup EXIT
pass=0; fail=0
ok()   { echo "  PASS  $1"; pass=$((pass+1)); }
bad()  { echo "  FAIL  $1"; fail=$((fail+1)); }

echo "### 0. datastores + build"
docker compose up -d >/dev/null 2>&1 || { echo "docker compose failed (is Docker running?)"; exit 1; }
cargo build >/tmp/nf_build.log 2>&1 || { echo "build failed:"; tail -20 /tmp/nf_build.log; exit 1; }
# fresh, deterministic state
docker exec natforge-postgres psql -U natforge -d natforge -c "DROP SCHEMA public CASCADE; CREATE SCHEMA public;" >/dev/null 2>&1
docker exec natforge-redis redis-cli FLUSHALL >/dev/null 2>&1

echo "### 1. origins (stand-ins for the user's local services)"
mkdir -p /tmp/nf_http; echo "HELLO-OVER-HTTP-SUBDOMAIN" > /tmp/nf_http/index.html
python3 -m http.server 8000 --directory /tmp/nf_http >/tmp/nf_o_http.log 2>&1 & PIDS+=($!)
openssl req -x509 -newkey rsa:2048 -keyout /tmp/nf_k.pem -out /tmp/nf_c.pem -days 1 -nodes -subj '/CN=origin.local' >/dev/null 2>&1
openssl s_server -accept 9443 -www -cert /tmp/nf_c.pem -key /tmp/nf_k.pem >/tmp/nf_o_https.log 2>&1 & PIDS+=($!)
python3 -c "import socket
s=socket.socket(); s.setsockopt(socket.SOL_SOCKET,socket.SO_REUSEADDR,1); s.bind(('127.0.0.1',25565)); s.listen()
while True:
    c,_=s.accept(); c.sendall(b'MC-OK\n'); c.close()" >/tmp/nf_o_tcp.log 2>&1 & PIDS+=($!)

start_planes() {
  RUST_LOG=warn ./target/debug/website_backend >/tmp/nf_web.log 2>&1 & PIDS+=($!)
  # HTTP_PORT is 18080 (not the 80/dev-8080 default) purely to dodge a port clash
  # with unrelated local containers on this dev box; the data plane is port-agnostic.
  PUBLIC_HOST=natforge.com CONTROL_ENDPOINT=127.0.0.1:4000 NODE_NAME=Local NODE_REGION=Local \
    HTTP_PORT=18080 HTTPS_PORT=8443 RUST_LOG=warn \
    ./target/debug/core_proxy_backend >/tmp/nf_core.log 2>&1 & PIDS+=($!)
  for i in $(seq 1 60); do
    [ "$(curl -s -o /dev/null -w '%{http_code}' 127.0.0.1:3000/ 2>/dev/null)" = "303" ] && \
    [ "$(curl -s -o /dev/null -w '%{http_code}' 127.0.0.1:3001/health 2>/dev/null)" = "200" ] && return 0
    sleep 0.4
  done
  echo "planes did not become ready"; tail -5 /tmp/nf_web.log /tmp/nf_core.log; exit 1
}
echo "### 2. start control + data planes"
start_planes

echo "### 3. register + reserve a 3-route tunnel + launch agent"
TOK=$(curl -s 127.0.0.1:3000/api/auth/register -H 'content-type: application/json' \
  -d '{"email":"evan@natforge.com","password":"hunter2pass"}' | jq -r '.token//empty')
RSV=$(curl -s 127.0.0.1:3000/api/tunnels/request -H "authorization: Bearer $TOK" -H 'content-type: application/json' \
  -d '{"routes":[{"mode":"http","local_port":8000},{"mode":"https","local_port":9443},{"mode":"tcp","local_port":25565}]}')
SUB=$(echo "$RSV"|jq -r .subdomain); TID=$(echo "$RSV"|jq -r .tunnel_id)
TCP_PORT=$(echo "$RSV"|jq -r '.routes[]|select(.mode=="tcp")|.public_endpoint'|sed 's/.*://')
./target/debug/proxy_node service-host --token "$TOK" \
  --route 8000:http --route 9443:https --route 25565:tcp >/tmp/nf_agent.log 2>&1 & PIDS+=($!)
sleep 3
echo "  reserved subdomain=$SUB tcp_port=$TCP_PORT"

echo "### 4. verifications"
[ "$(curl -s -H "Host: $SUB.natforge.com" http://127.0.0.1:18080/)" = "HELLO-OVER-HTTP-SUBDOMAIN" ] \
  && ok "HTTP via subdomain (shared :18080)" || bad "HTTP via subdomain"
[ "$(curl -s -o /dev/null -w '%{http_code}' -H 'Host: nope.natforge.com' http://127.0.0.1:18080/)" = "404" ] \
  && ok "unknown subdomain -> 404" || bad "unknown subdomain -> 404"
curl -sk --resolve "$SUB.natforge.com:8443:127.0.0.1" "https://$SUB.natforge.com:8443/" | grep -q 's_server' \
  && ok "HTTPS via SNI passthrough (origin cert, :8443)" || bad "HTTPS via SNI passthrough"
[ "$(timeout 2 bash -c "exec 3<>/dev/tcp/127.0.0.1/$TCP_PORT; head -c5 <&3" 2>/dev/null)" = "MC-OK" ] \
  && ok "raw TCP via dedicated port $TCP_PORT" || bad "raw TCP via dedicated port"

# second user sharing :18080
T2=$(curl -s 127.0.0.1:3000/api/auth/register -H 'content-type: application/json' -d '{"email":"x@y.z","password":"secondpass"}' | jq -r '.token//empty')
R2=$(curl -s 127.0.0.1:3000/api/tunnels/request -H "authorization: Bearer $T2" -H 'content-type: application/json' -d '{"routes":[{"mode":"http","local_port":8000}]}')
SUB2=$(echo "$R2"|jq -r .subdomain)
./target/debug/proxy_node service-host --token "$T2" --route 8000:http >/tmp/nf_agent2.log 2>&1 & PIDS+=($!)
sleep 3
{ [ "$(curl -s -H "Host: $SUB.natforge.com" http://127.0.0.1:18080/)" = "HELLO-OVER-HTTP-SUBDOMAIN" ] && \
  [ "$(curl -s -H "Host: $SUB2.natforge.com" http://127.0.0.1:18080/)" = "HELLO-OVER-HTTP-SUBDOMAIN" ]; } \
  && ok "two users share :18080 by distinct subdomain" || bad "two users share :18080"

# multi-route over one session (http + tcp)
{ [ "$(curl -s -H "Host: $SUB.natforge.com" http://127.0.0.1:18080/)" = "HELLO-OVER-HTTP-SUBDOMAIN" ] && \
  [ "$(timeout 2 bash -c "exec 3<>/dev/tcp/127.0.0.1/$TCP_PORT; head -c5 <&3" 2>/dev/null)" = "MC-OK" ]; } \
  && ok "multi-route (http+tcp) over one yamux session" || bad "multi-route over one session"

echo "### 4b. regions, per-tunnel bandwidth + logging, per-tunnel geo-block"
sleep 3   # let a bandwidth tick (5s) land and connection-log POSTs flush
curl -s 127.0.0.1:3000/api/regions -H "authorization: Bearer $TOK" | jq -e '.[]|select(.name=="Local")' >/dev/null \
  && ok "region list exposes the self-registered Local node" || bad "region list"
[ "$(curl -s 127.0.0.1:3000/api/tunnels/$TID/bandwidth -H "authorization: Bearer $TOK" | jq '.series|length')" -ge 1 ] \
  && ok "per-tunnel bandwidth series recorded" || bad "per-tunnel bandwidth series"
[ "$(curl -s 127.0.0.1:3000/api/tunnels/$TID/logs -H "authorization: Bearer $TOK" | jq 'length')" -ge 1 ] \
  && ok "per-tunnel connection log records connections" || bad "per-tunnel connection log"
curl -s -X PUT 127.0.0.1:3000/api/tunnels/$TID/region_blocks -H "authorization: Bearer $TOK" \
  -H 'content-type: application/json' -d '{"country_codes":["cn","de"]}' >/dev/null
[ "$(curl -s 127.0.0.1:3000/api/tunnels/$TID/region_blocks -H "authorization: Bearer $TOK" | jq -rc '.')" = '["CN","DE"]' ] \
  && ok "per-tunnel country block list persists" || bad "per-tunnel country block"

# Control channel is real TLS: the port presents the self-signed cert whose
# SHA-256 the reservation pinned (agents connect only if this fingerprint matches).
RSV_FP=$(echo "$RSV" | jq -r '.control_cert_fingerprint // empty')
CERT_FP=$(echo | openssl s_client -connect 127.0.0.1:4000 2>/dev/null \
  | openssl x509 -noout -fingerprint -sha256 2>/dev/null | sed 's/.*=//; s/://g' | tr 'A-Z' 'a-z')
{ [ -n "$RSV_FP" ] && [ "$RSV_FP" = "$CERT_FP" ]; } \
  && ok "control port serves TLS with the pinned cert" || bad "control TLS / cert pin (rsv=$RSV_FP cert=$CERT_FP)"

echo "### 5. state survives a both-planes restart"
before=$(curl -s 127.0.0.1:3000/api/tunnels -H "authorization: Bearer $TOK" | jq -rc '.[0]|{subdomain,tcp:(.routes[]|select(.mode=="tcp")|.public_endpoint)}')
kill %2 2>/dev/null; pkill -9 -f '[t]arget/debug/website_backend' 2>/dev/null; pkill -9 -f '[t]arget/debug/core_proxy_backend' 2>/dev/null
sleep 2; start_planes; sleep 5
after=$(curl -s 127.0.0.1:3000/api/tunnels -H "authorization: Bearer $TOK" | jq -rc '.[0]|{subdomain,tcp:(.routes[]|select(.mode=="tcp")|.public_endpoint)}')
[ "$before" = "$after" ] && [ -n "$before" ] \
  && ok "same subdomain+port after restart ($after)" || bad "state survives restart (before=$before after=$after)"
[ "$(curl -s -H "Host: $SUB.natforge.com" http://127.0.0.1:18080/)" = "HELLO-OVER-HTTP-SUBDOMAIN" ] \
  && ok "HTTP works again after restart" || bad "HTTP after restart"

echo
echo "### RESULT: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
