#!/usr/bin/env bash
set -u
cd "$(dirname "$0")"
REPO=${REPO:-$(cd .. && pwd)}
BIN=${BIN:-$REPO/target/release}
ROUNDS=${ROUNDS:-12}
export DATABASE_URL=postgres://natforge:natforge@127.0.0.1:55432/natforge
export REDIS_URL=redis://127.0.0.1:56379

if ss -Hltn | grep -qE ':(3000|3001|4000|7000|9000|18080|18090|55432|56379)\b'; then
  echo "a benchmark port is already in use, stop the running stack first"
  exit 1
fi

pids=()
stop() {
  kill "${pids[@]}" 2>/dev/null
  wait 2>/dev/null
  docker rm -f nfbench-pg nfbench-redis >/dev/null 2>&1
}
trap stop EXIT

docker rm -f nfbench-pg nfbench-redis >/dev/null 2>&1
docker run -d --name nfbench-pg -p 127.0.0.1:55432:5432 \
  -e POSTGRES_USER=natforge -e POSTGRES_PASSWORD=natforge -e POSTGRES_DB=natforge postgres:16-alpine >/dev/null
docker run -d --name nfbench-redis -p 127.0.0.1:56379:6379 redis:7-alpine >/dev/null
until docker exec nfbench-pg pg_isready -h 127.0.0.1 -U natforge >/dev/null 2>&1; do sleep 0.5; done

./bench serve 9000 & pids+=($!)
( cd "$REPO" && exec "$BIN/natforge-backend" ) >/dev/null 2>&1 & pids+=($!)
PUBLIC_HOST=natforge.com CONTROL_ENDPOINT=127.0.0.1:4000 HTTP_PORT=18080 HTTPS_PORT=8443 \
  NODE_NAME=Local NODE_REGION=Local "$BIN/natforge-node" >/dev/null 2>&1 & node=$!; pids+=($node)
until curl -s 127.0.0.1:3000/ >/dev/null && curl -s 127.0.0.1:3001/health >/dev/null; do sleep 0.5; done

json='content-type: application/json'
token=$(curl -s 127.0.0.1:3000/api/auth/register -H "$json" \
        -d '{"email":"b@n.com","password":"benchpass1"}' | jq -r .token)
sub=$(curl -s 127.0.0.1:3000/api/tunnels/request -H "authorization: Bearer $token" -H "$json" \
        -d '{"routes":[{"mode":"http","local_port":9000}]}' | jq -r .subdomain)
"$BIN/natforge" service-host --token "$token" --route 9000:http >/dev/null 2>&1 & agent=$!; pids+=($agent)

printf 'bindPort = 7000\nvhostHTTPPort = 18090\n' > /tmp/frps.toml
printf 'serverAddr = "127.0.0.1"\nserverPort = 7000\ntransport.tls.enable = true\n[[proxies]]\nname = "o"\ntype = "http"\nlocalPort = 9000\ncustomDomains = ["frp.natforge.com"]\n' > /tmp/frpc.toml
./frps -c /tmp/frps.toml >/dev/null 2>&1 & frps=$!; pids+=($frps)
sleep 1
./frpc -c /tmp/frpc.toml >/dev/null 2>&1 & frpc=$!; pids+=($frpc)

until [ "$(curl -s 127.0.0.1:18080/64 -H "Host: $sub.natforge.com" | wc -c)" = 64 ] &&
      [ "$(curl -s 127.0.0.1:18090/64 -H "Host: frp.natforge.com" | wc -c)" = 64 ]; do sleep 0.5; done

kb() { awk '/^VmRSS/{print $2}' /proc/"$1"/status; }

echo "system,round,second,process1_kb,process2_kb,total_mb" > memory.csv
measure() {
  local system=$1 addr=$2 host=$3 p1=$4 p2=$5 round second load k1 k2
  for round in $(seq "$ROUNDS"); do
    ./bench load "$addr" "$host" /64 100 16 >/dev/null & load=$!
    for second in $(seq 12); do
      sleep 1
      k1=$(kb "$p1"); k2=$(kb "$p2")
      echo "$system,$round,$second,$k1,$k2,$(( (k1 + k2 + 512) / 1024 ))" >> memory.csv
    done
    wait "$load"
    sleep 3
  done
}
measure natforge 127.0.0.1:18080 "$sub.natforge.com" "$node" "$agent"
measure frp 127.0.0.1:18090 frp.natforge.com "$frps" "$frpc"

summary() {
  grep "^$1," memory.csv | cut -d, -f6 | sort -n | awk -v s="$1" '
    { v[NR] = $1 }
    END {
      m = NR % 2 ? v[(NR + 1) / 2] : (v[NR / 2] + v[NR / 2 + 1]) / 2
      printf "memory,%s,,,,,,,,%.0f,%s,%s\n", s, m, v[1], v[NR]
    }'
}
grep -v '^memory,' results.csv > results.tmp
summary natforge >> results.tmp
summary frp >> results.tmp
mv results.tmp results.csv
grep '^memory,' results.csv
