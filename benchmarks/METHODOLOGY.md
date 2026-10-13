# How NatForge and frp were benchmarked

This folder holds the benchmark **inputs, results, and figures**. It is documentation,
not a test harness: nothing here runs automatically and it is not wired into CI. To
reproduce the numbers, follow the manual steps below, one command block at a time.

## What is measured
The relay's request latency (median and tail), bulk throughput, and memory footprint.
Three systems are compared on identical hardware:

- **direct** (no tunnel), as a baseline,
- **NatForge**,
- **frp** (an open-source reverse tunneler, used as a comparison baseline).

## Setup (loopback, one machine)
Everything runs as local processes on a single host over `127.0.0.1`, so the numbers
measure the relay software itself, not a variable internet path. The **same origin** and
the **same load generator** drive all three targets; only the address (and Host header)
differ, which is what makes the comparison fair.

- **origin**: a small fixed-response HTTP server standing in for the user's local
  service. Its response size comes from the URL path (`/64` = a 64-byte body for latency,
  `/10485760` = 10 MiB for throughput). It is fast enough never to be the bottleneck.
- **load generator**: opens N keep-alive connections (one thread each), sends
  back-to-back requests for a fixed duration, and records per-request latency,
  requests/sec, and throughput.
- **frp** runs with TLS explicitly enabled on its client/server leg
  (`transport.tls.enable = true`, which is also frp's default), so it encrypts its
  tunnel just as NatForge does. Verified: frp's throughput is the same with TLS on or
  by default (~1100 MiB/s) and only faster with TLS off (~1555), so the comparison is
  a fair encrypted-vs-encrypted one.

The origin and load generator are one small plain-`std` Rust file (`bench.rs`, no async,
no external crates): `bench serve` is the origin, `bench load` is the client. They are
fast enough that the measuring tools are never the bottleneck (a `curl`-only version was
tried and is too coarse: it measures sub-millisecond latency as noise).

`bench load` takes positional arguments: `bench load <addr> <host-header> <path> <connections> <seconds>`
and prints one CSV fragment: `connections,requests,rps,mib_s,p50_ms,p95_ms,p99_ms`.

## Testbed
Intel Core i5-11320H (8 threads), single host, loopback. PostgreSQL + Redis run via
`docker compose`; the NatForge node and agent are release builds.

Loopback isolates the relay software. Real-network latency over the deployed VM (the true
CGNAT path, residential uplink) is a separate measurement.

## Files in this folder
- `bench.rs` - origin + load generator, one plain-`std` Rust file.
- `memory.sh` - the memory measurement (step 12).
- `plot.py` - draws the three charts from `results.csv`.
- `results.csv` - the measured numbers (already filled in from the run in `RESULTS.md`).
- `memory.csv` - every raw memory sample from that run.
- `fig1_latency.png`, `fig2_throughput.png`, `fig3_memory.png` - the charts.
- `RESULTS.md` - the numbers written up as tables.
- `METHODOLOGY.md` - this file.

The frp binaries (`frps`, `frpc`) and the compiled `bench` are **not** kept here; the
steps below say how to fetch/build them.

## Reproduce (run these by hand, block by block)

Prerequisites: a Rust toolchain, Docker, Python 3 with `venv`, and `jq` + `curl`.
Set `REPO` to your checkout of the NatForge source tree.

```sh
REPO=/home/you/Natforge-Thesis      # <-- your checkout
cd "$REPO/benchmarks"                    # this folder (lives inside the repo)
```

**1. Build the NatForge release binaries.**
```sh
( cd "$REPO" && cargo build --release --workspace )
```

**2. Download the frp comparison baseline (v0.71.0) into this folder, once.**
```sh
curl -fsSL -o frp.tgz \
  https://github.com/fatedier/frp/releases/download/v0.71.0/frp_0.71.0_linux_amd64.tar.gz
tar xzf frp.tgz
cp frp_0.71.0_linux_amd64/frps frp_0.71.0_linux_amd64/frpc .
rm -rf frp.tgz frp_0.71.0_linux_amd64
```

**3. Compile the origin + load generator (no crates, bare `rustc`).**
```sh
rustc -O bench.rs -o bench
```

**4. Start PostgreSQL + Redis and reset to a clean schema.**
```sh
( cd "$REPO" && docker compose up -d )
docker exec natforge-postgres psql -U natforge -d natforge \
  -c "DROP SCHEMA public CASCADE; CREATE SCHEMA public;"
docker exec natforge-redis redis-cli FLUSHALL
```

**5. Start the origin and both NatForge planes (backgrounded here; separate terminals also work).**
```sh
./bench serve 9000 &
( cd "$REPO" && ./target/release/natforge-backend ) &
( cd "$REPO" && PUBLIC_HOST=natforge.com CONTROL_ENDPOINT=127.0.0.1:4000 \
    HTTP_PORT=18080 HTTPS_PORT=8443 NODE_NAME=Local NODE_REGION=Local \
    ./target/release/natforge-node ) &
until curl -s 127.0.0.1:3000/ >/dev/null && curl -s 127.0.0.1:3001/health >/dev/null; do sleep 0.5; done
```

**6. Register a user, reserve an HTTP tunnel to the origin, and start the agent.**
```sh
json='content-type: application/json'
token=$(curl -s 127.0.0.1:3000/api/auth/register -H "$json" \
        -d '{"email":"b@n.com","password":"benchpass1"}' | jq -r .token)
sub=$(curl -s 127.0.0.1:3000/api/tunnels/request -H "authorization: Bearer $token" -H "$json" \
        -d '{"routes":[{"mode":"http","local_port":9000}]}' | jq -r .subdomain)
echo "subdomain = $sub"
( cd "$REPO" && ./target/release/natforge service-host --token "$token" --route 9000:http ) &
```

**7. Start frp pointing at the same origin (TLS on, so it encrypts like NatForge).**
```sh
printf 'bindPort = 7000\nvhostHTTPPort = 18090\n' > /tmp/frps.toml
printf 'serverAddr = "127.0.0.1"\nserverPort = 7000\ntransport.tls.enable = true\n[[proxies]]\nname = "o"\ntype = "http"\nlocalPort = 9000\ncustomDomains = ["frp.natforge.com"]\n' > /tmp/frpc.toml
./frps -c /tmp/frps.toml &
sleep 1
./frpc -c /tmp/frpc.toml &
sleep 3
```

**8. Sanity-check each path returns 64 bytes.**
```sh
curl -s 127.0.0.1:9000/64                                | wc -c   # direct
curl -s 127.0.0.1:18080/64 -H "Host: $sub.natforge.com"  | wc -c   # NatForge
curl -s 127.0.0.1:18090/64 -H "Host: frp.natforge.com"   | wc -c   # frp
```

**9. Latency sweep: a 64-byte reply, 8 s per point, at rising concurrency.**
```sh
csv=results.csv
echo "test,system,conns,requests,rps,mib_s,p50_ms,p95_ms,p99_ms,rss_mb,rss_min,rss_max" > "$csv"
for conns in 1 10 50 100 200; do
  echo "latency,direct,$(  ./bench load 127.0.0.1:9000  x                  /64 "$conns" 8)," >> "$csv"
  echo "latency,natforge,$(./bench load 127.0.0.1:18080 "$sub.natforge.com" /64 "$conns" 8)," >> "$csv"
  echo "latency,frp,$(     ./bench load 127.0.0.1:18090 frp.natforge.com   /64 "$conns" 8)," >> "$csv"
done
```

**10. Throughput: 10 MiB replies, back to back over 4 connections for 8 s.**
```sh
echo "throughput,direct,$(  ./bench load 127.0.0.1:9000  x                  /10485760 4 8)," >> "$csv"
echo "throughput,natforge,$(./bench load 127.0.0.1:18080 "$sub.natforge.com" /10485760 4 8)," >> "$csv"
echo "throughput,frp,$(     ./bench load 127.0.0.1:18090 frp.natforge.com   /10485760 4 8)," >> "$csv"
```

**11. Stop everything started in steps 5-7.**
`pkill -x` cannot match `natforge-backend` because Linux cuts process names to 15
characters, so the backend and the origin are matched by command line instead.
```sh
pkill -x natforge-node; pkill -f "natforge service-host"; pkill -f target/release/natforge-backend
pkill -x frps; pkill -x frpc; pkill -f "bench serve"
```

**12. Memory: RSS of each relay's two processes, 12 rounds of 12 samples.**
`memory.sh` starts its own fresh copy of the steps 5-7 stack, with a throwaway PostgreSQL
and Redis on ports 55432 and 56379 so your own database is not touched. It then runs 12
rounds of the same 100-connection `/64` load against each relay. Each round lasts 16 s and
is sampled once per second for 12 s, taking the summed VmRSS of `natforge-node` + agent, or
of `frps` + `frpc`: 144 samples per relay. Sampling across rounds as well as within one
matters: NatForge's RSS is flat inside a round but steps up as each new batch of
connections arrives (freed memory stays with the allocator instead of going back to the
OS), while frp's moves with its garbage collector from second to second. Every sample goes
to `memory.csv`; the median, min and max go to the `memory` rows of `results.csv`.
```sh
bash memory.sh
```

**13. Draw the charts (needs matplotlib; use a venv if system Python is externally managed).**
```sh
python3 -m venv venv && venv/bin/pip install matplotlib
venv/bin/python plot.py     # writes fig1_latency.png, fig2_throughput.png, fig3_memory.png
```
