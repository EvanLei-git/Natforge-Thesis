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

## Throughput (single 10 MiB transfer, 4 streams)

| system   | MiB/s | Gbit/s |
|----------|------:|-------:|
| direct   | 3427  | 28.7   |
| NatForge | 1203  | 10.1   |
| frp      | 1078  | 9.0    |

With 64 KiB relay copy buffers and larger yamux send frames, NatForge now edges out
frp on raw loopback throughput as well. All three far exceed any real uplink; in a
real deployment throughput is bounded by the network link, not the relay.

## Memory footprint (RSS under 100-connection load)

Sampled once per second over a sustained 100-connection load (12 samples each):

| system   | median | range (min-max) |
|----------|-------:|----------------:|
| NatForge | 40 MB  | 40 - 40 MB      |
| frp      | 226 MB | 142 - 365 MB    |

The two behave completely differently, and the range matters more than any single number.
NatForge (Rust, no garbage collector) holds a **flat** 40 MB: all 12 samples read exactly
40 MB. frp (Go) **oscillates** between 142 and 365 MB second to second as its garbage
collector grows the heap and then returns memory to the OS, so any single reading (earlier
runs happened to catch 178 and 310) is just a snapshot of that sawtooth. Taken as a range,
frp uses roughly 4-9x NatForge's memory; and for a long-running relay the more important
property is that NatForge's footprint is small *and predictable*.

Figures: fig1 (latency: solid = median, dashed = p99 tail), fig2 (throughput), fig3 (memory).
Loopback isolates the relay software; real-network (CGNAT) latency over the deployed
VM is a separate measurement.
