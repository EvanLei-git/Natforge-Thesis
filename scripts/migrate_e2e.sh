#!/usr/bin/env bash
# Local end-to-end test for cross-region migration: two core nodes + one website;
# a tunnel reserved on node1 is migrated to node2 and the agent follows.
set -u
cd "$(dirname "$0")/.."
source "$HOME/.cargo/env" 2>/dev/null || true
PIDS=(); cleanup(){ for p in "${PIDS[@]:-}"; do kill "$p" 2>/dev/null; done; wait 2>/dev/null; }
trap cleanup EXIT

cargo build >/tmp/nf_mig_build.log 2>&1 || { echo BUILD-FAIL; tail -20 /tmp/nf_mig_build.log; exit 1; }
docker exec natforge-postgres psql -U natforge -d natforge -c "DROP SCHEMA public CASCADE; CREATE SCHEMA public;" >/dev/null 2>&1
docker exec natforge-redis redis-cli FLUSHALL >/dev/null 2>&1

echo "MIGRATE-OK" > /tmp/nf_mig_index; mkdir -p /tmp/nf_mig_http; cp /tmp/nf_mig_index /tmp/nf_mig_http/index.html
python3 -m http.server 8000 --directory /tmp/nf_mig_http >/tmp/nf_mig_o.log 2>&1 & PIDS+=($!)

RUST_LOG=warn ./target/debug/natforge-backend >/tmp/nf_mig_web.log 2>&1 & PIDS+=($!)
# node1 (US) and node2 (EU): disjoint control/internal/http/https ports + port pools.
NODE_ID=node1 NODE_NAME=Node1 NODE_REGION=US PUBLIC_HOST=natforge.com \
  CONTROL_ENDPOINT=127.0.0.1:4000 CORE_CONTROL_PORT=4000 CORE_INTERNAL_PORT=3001 \
  HTTP_PORT=18080 HTTPS_PORT=8443 INTERNAL_URL=http://127.0.0.1:3001 \
  PUBLIC_PORT_MIN=20000 PUBLIC_PORT_MAX=20050 RUST_LOG=warn \
  ./target/debug/natforge-node >/tmp/nf_mig_n1.log 2>&1 & PIDS+=($!)
NODE_ID=node2 NODE_NAME=Node2 NODE_REGION=EU PUBLIC_HOST=n2.local \
  CONTROL_ENDPOINT=127.0.0.1:4001 CORE_CONTROL_PORT=4001 CORE_INTERNAL_PORT=3002 \
  HTTP_PORT=18081 HTTPS_PORT=8444 INTERNAL_URL=http://127.0.0.1:3002 \
  PUBLIC_PORT_MIN=20051 PUBLIC_PORT_MAX=20100 RUST_LOG=warn \
  ./target/debug/natforge-node >/tmp/nf_mig_n2.log 2>&1 & PIDS+=($!)

ready=0; for i in $(seq 1 60); do
  [ "$(curl -s -o /dev/null -w '%{http_code}' 127.0.0.1:3000/ 2>/dev/null)" = "303" ] && \
  [ "$(curl -s -o /dev/null -w '%{http_code}' 127.0.0.1:3001/health 2>/dev/null)" = "200" ] && \
  [ "$(curl -s -o /dev/null -w '%{http_code}' 127.0.0.1:3002/health 2>/dev/null)" = "200" ] && { ready=1; break; }; sleep 0.4; done
[ "$ready" = 1 ] || { echo PLANES-NOT-READY; tail -6 /tmp/nf_mig_web.log /tmp/nf_mig_n1.log /tmp/nf_mig_n2.log; exit 1; }
echo "regions: $(curl -s 127.0.0.1:3000/api/regions -H "authorization: Bearer $(curl -s 127.0.0.1:3000/api/auth/register -H 'content-type: application/json' -d '{"email":"probe@x.io","password":"probepass1"}' | jq -r .token)" | jq -c '[.[].node_id]')"

TOK=$(curl -s 127.0.0.1:3000/api/auth/register -H 'content-type: application/json' -d '{"email":"mig@natforge.com","password":"hunter2pass"}' | jq -r '.token//empty')
RSV=$(curl -s 127.0.0.1:3000/api/tunnels/request -H "authorization: Bearer $TOK" -H 'content-type: application/json' \
  -d '{"routes":[{"mode":"http","local_port":8000}],"node_id":"node1"}')
TID=$(echo "$RSV"|jq -r .tunnel_id); SUB=$(echo "$RSV"|jq -r .subdomain)
echo "reserved tunnel $TID sub=$SUB on $(echo "$RSV"|jq -r .node_id) endpoint=$(echo "$RSV"|jq -r .control_endpoint)"
./target/debug/natforge service-host --token "$TOK" --route 8000:http >/tmp/nf_mig_agent.log 2>&1 & PIDS+=($!)
sleep 3

echo "### before migration (should serve on node1 :18080, not node2 :18081)"
[ "$(curl -s -H "Host: $SUB.natforge.com" http://127.0.0.1:18080/)" = "MIGRATE-OK" ] && echo "  PASS node1 serves" || echo "  FAIL node1 serves"
[ "$(curl -s -o /dev/null -w '%{http_code}' -H "Host: $SUB.natforge.com" http://127.0.0.1:18081/)" = "404" ] && echo "  PASS node2 does not" || echo "  FAIL node2 unexpectedly serves"

echo "### migrate $TID -> node2"
curl -s -X POST "127.0.0.1:3000/api/tunnels/$TID/migrate" -H "authorization: Bearer $TOK" -H 'content-type: application/json' -d '{"node_id":"node2"}'; echo
sleep 7
echo "--- agent log tail ---"; tail -4 /tmp/nf_mig_agent.log

echo "### after migration (should serve on node2 :18081, not node1 :18080)"
[ "$(curl -s -H "Host: $SUB.natforge.com" http://127.0.0.1:18081/)" = "MIGRATE-OK" ] && echo "  PASS node2 serves" || echo "  FAIL node2 serves"
[ "$(curl -s -o /dev/null -w '%{http_code}' -H "Host: $SUB.natforge.com" http://127.0.0.1:18080/)" = "404" ] && echo "  PASS node1 stopped serving" || echo "  FAIL node1 still serves"
echo "  agent reconnected to node2 endpoint? $(grep -q '127.0.0.1:4001' /tmp/nf_mig_agent.log && echo yes || echo no)"
echo DONE
