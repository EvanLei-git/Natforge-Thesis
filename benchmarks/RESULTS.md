# NatForge performance vs. frp (loopback comparison)

Testbed: Intel Core i5-11320H (8 threads), single host, loopback. Direct (no tunnel),
NatForge, and frp measured with the same origin and the same HTTP keep-alive load
harness; only the tunnel software differs. frp is an open-source reverse tunneler
used purely as a comparison baseline.

## Latency (median p50 / tail p99, milliseconds)

| conns | direct p50 | NatForge p50 | frp p50 | NatForge p99 | frp p99 |
|------:|-----------:|-------------:|--------:|-------------:|--------:|
| 1     | 0.010      | 0.053        | 0.091   | 0.095        | 0.189   |
| 10    | 0.024      | 0.183        | 0.349   | 0.415        | 1.381   |
| 50    | 0.143      | 0.722        | 2.492   | 1.771        | 17.704  |
| 100   | 0.287      | 1.396        | 3.483   | 3.271        | 32.700  |
| 200   | 0.588      | 2.613        | 7.721   | 5.794        | 64.885  |

NatForge has lower latency than frp at every concurrency level, and its tail (p99)
stays single-digit-ms where frp climbs to 18-65 ms.

## Throughput (10 MiB replies over 4 connections, 8 s)

| system   | MiB/s | Gbit/s |
|----------|------:|-------:|
| direct   | 3427  | 28.7   |
| NatForge | 1203  | 10.1   |
| frp      | 1078  | 9.0    |

With 64 KiB relay copy buffers and larger yamux send frames, NatForge now edges out
frp on raw loopback throughput as well. All three far exceed any real uplink; in a
real deployment throughput is bounded by the network link, not the relay.

## Memory footprint (RSS under 100-connection load)

Measured by `memory.sh`: starting from fresh processes, 12 rounds of the same
100-connection load (16 s each), sampled once per second for 12 s of every round
(144 samples each). Raw samples are in `memory.csv`.

| system   | median | range (min-max) |
|----------|-------:|----------------:|
| NatForge | 58 MB  | 26 - 63 MB      |
| frp      | 192 MB | 123 - 407 MB    |

The two vary for different reasons. NatForge (Rust, no garbage collector) does not move
at all within a round: all 12 samples of every round are identical. Each new batch of
connections steps it up instead (26 MB in the first round, 40 in the second, 47 in the
third) until it levels off at 57-63 MB from the sixth round on. Memory freed when
connections close stays with the allocator rather than going back to the OS, so the RSS
tracks the most the relay has needed so far; it stops climbing, so this is not a leak.
frp (Go) swings by 110-270 MB inside every round as its garbage collector grows the heap
and returns memory to the OS, so any single reading is a snapshot of that sawtooth.
Comparing medians, frp uses about 3.3x NatForge's memory, and NatForge's footprint is the
predictable one.

Figures: fig1 (latency: solid = median, dashed = p99 tail), fig2 (throughput), fig3 (memory).
Loopback isolates the relay software; real-network (CGNAT) latency over the deployed
VM is a separate measurement.
