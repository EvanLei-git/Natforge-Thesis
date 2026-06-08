# NatForge — Synthesized Implementation Plan

## Wire Protocol

=== CONTROL-PLANE HANDSHAKE (unchanged framing) ===
Same length-prefixed framing as today (proxy_node/src/protocol.rs): u32 big-endian length, then JSON, one frame each direction, MAX_FRAME = 1<<20, read with read_u32 + read_exact, write with write_u32 + write_all + flush. After the two frames the SAME socket is handed to yamux (core = Mode::Client, agent = Mode::Server) — unchanged.

--- Frame 1: agent -> core (AgentHello, JSON) ---
{
  "tunnel_token": "<JWT>",
  "role": "service_host",
  "routes": [
    { "route_id": 1, "local_port": 8000 },
    { "route_id": 2, "local_port": 8443 },
    { "route_id": 3, "local_port": 25565 }
  ]
}
Reconciliation rule in handle_agent: every binding.route_id MUST exist in the token's claims.routes; an unknown route_id => CoreReply::Error and abort (release any ports already bound). A token route with no matching binding is simply not exposed (allowed). local_port re-checked against blocked_ports (defense in depth).

--- Frame 2: core -> agent (CoreReply, JSON) ---
OK:
{
  "status": "ok",
  "tunnel_id": 42,
  "subdomain": "duck-a1b2",
  "routes": [
    { "route_id": 1, "mode": "http",  "public_endpoint": "duck-a1b2.natforge.com:80"  },
    { "route_id": 2, "mode": "https", "public_endpoint": "duck-a1b2.natforge.com:443" },
    { "route_id": 3, "mode": "tcp",   "public_endpoint": "natforge.com:20007" }
  ]
}
ERROR:
{ "status": "error", "message": "route 3 not authorized" }

=== PER-STREAM BINARY PREAMBLE (NEW; core writes, agent reads, before any payload) ===
Written by core as the FIRST bytes of EVERY yamux outbound stream it opens. Dependency-free (no `bytes` crate). All multi-byte ints big-endian.

Offset  Size  Field
0       4     magic = b"NFS1"            (NatForge Stream v1)
4       1     version = 1
5       2     route_id : u16             (agent maps route_id -> local_port)
7       1     client_addr_kind : 0=none, 4=IPv4, 6=IPv6
8       L     client_ip bytes           (L = 0 / 4 / 16 per kind)
8+L     2     client_port : u16
10+L    2     replay_len : u16           (count of replayed payload bytes that FOLLOW; 0 for raw tcp)
12+L    R     replay bytes               (the peeked HTTP request / TLS ClientHello, R = replay_len)
then    ...   live bidirectional traffic (copy_bidirectional)

Fixed header size for IPv4 peer: 16 bytes + replay. For tcp routes replay_len = 0 (header only, then live bytes). For http/https routes replay carries the exact peeked bytes so the agent's local service sees the byte-exact original request/ClientHello.

Encoder (natforge-proto):
pub fn encode_preamble(route_id: u16, peer: Option<SocketAddr>, replay: &[u8]) -> Vec<u8> {
    debug_assert!(replay.len() <= u16::MAX as usize);   // peek cap (8 KiB) guarantees this
    let mut b = Vec::with_capacity(16 + replay.len());
    b.extend_from_slice(b"NFS1"); b.push(1u8);
    b.extend_from_slice(&route_id.to_be_bytes());
    match peer.map(|p| p.ip()) {
        Some(IpAddr::V4(v4)) => { b.push(4); b.extend_from_slice(&v4.octets()); }
        Some(IpAddr::V6(v6)) => { b.push(6); b.extend_from_slice(&v6.octets()); }
        None => { b.push(0); }
    }
    b.extend_from_slice(&peer.map(|p| p.port()).unwrap_or(0).to_be_bytes());
    b.extend_from_slice(&(replay.len() as u16).to_be_bytes());
    b.extend_from_slice(replay);
    b
}

Decoder (natforge-proto):
pub async fn read_preamble<R: AsyncRead + Unpin>(r: &mut R)
    -> anyhow::Result<(u16, Option<SocketAddr>, Vec<u8>)> {
    let mut hdr = [0u8; 7]; r.read_exact(&mut hdr).await?;
    anyhow::ensure!(&hdr[0..4] == b"NFS1" && hdr[4] == 1, "bad stream preamble");
    let route_id = u16::from_be_bytes([hdr[5], hdr[6]]);
    let mut kind = [0u8;1]; r.read_exact(&mut kind).await?;
    let ip = match kind[0] {
        4 => { let mut o=[0u8;4];  r.read_exact(&mut o).await?; Some(IpAddr::from(o)) }
        6 => { let mut o=[0u8;16]; r.read_exact(&mut o).await?; Some(IpAddr::from(o)) }
        _ => None,
    };
    let mut p=[0u8;2]; r.read_exact(&mut p).await?; let port=u16::from_be_bytes(p);
    let mut rl=[0u8;2]; r.read_exact(&mut rl).await?; let replay_len=u16::from_be_bytes(rl) as usize;
    let mut replay = vec![0u8; replay_len];
    if replay_len>0 { r.read_exact(&mut replay).await?; }
    Ok((route_id, ip.map(|i| SocketAddr::new(i, port)), replay))
}

=== AGENT handle_stream (proxy_node/src/service_host.rs) ===
async fn handle_stream(stream: yamux::Stream, routes: Arc<HashMap<u16,u16>>) {
    let mut remote = stream.compat();
    let (route_id, _peer, replay) = match natforge_proto::read_preamble(&mut remote).await {
        Ok(v) => v, Err(e) => { warn!("preamble: {e}"); return; }
    };
    let Some(&local_port) = routes.get(&route_id) else { warn!("unknown route {route_id}"); return; };
    let mut local = match TcpStream::connect(("127.0.0.1", local_port)).await {
        Ok(s)=>s, Err(e)=>{ error!("dial 127.0.0.1:{local_port}: {e}"); return; }
    };
    if !replay.is_empty() { if local.write_all(&replay).await.is_err() { return; } }
    if let Err(e) = copy_bidirectional(&mut remote, &mut local).await { warn!("relay closed: {e}"); }
}

=== HAND-ROLLED HTTP Host SCAN (core_proxy_backend/src/tunnel/shared.rs) ===
Read into buf (cap = config.max_header_bytes = 16384) until b"\r\n\r\n" present OR cap hit. Find first line matching case-insensitive "host:", value before any ':' port and trailing '.'; subdomain = leftmost label after stripping the configured apex. No Host / malformed => write "HTTP/1.1 400 ..." and close (do NOT open a stream).

=== HAND-ROLLED TLS SNI PARSER (passthrough, no termination) ===
enum SniParse { Found(String), NoSni, NotTls, NeedMore }
Walk: require b[0]==0x16 (handshake) && b[1]==0x03; rec_len=u16(b[3..5]); buffer until 5+rec_len present (cap 8192). Handshake h: h[0]==0x01 (ClientHello); skip 2 (legacy_version)+32 (random); skip 1+session_id; skip 2+cipher_suites; skip 1+compression; read 2 ext_total; loop extensions; ext type 0x0000 (server_name) => list_len(2), name_type(1)==0x00, name_len(2), name (UTF-8). Every index access bounds-checked; any anomaly => NeedMore (read more) or NoSni/NotTls (close). No SNI / non-TLS / unknown subdomain => close (cannot 404 under TLS).

=== route_and_splice (shared for http + https) ===
1. resolve handle = state.routes.read().await.get(sub).filter(|h| h.mode == proto).cloned()
2. if None: Http => write 502/404 + close; Https => close
3. ddos.analyze_connection(peer.ip()) guard
4. open_tx.send(OpenStream{ route_id: handle.route_id, reply }) ; await stream
5. outbound.write_all(&encode_preamble(handle.route_id, Some(peer), &buf))   // peeked bytes ride INSIDE preamble replay
6. copy_bidirectional(&mut inbound, &mut outbound) ; bump handle.stats

=== RAW TCP dedicated port (fast path, no peek) ===
bridge_public_connection(inbound, peer, route_id, open_tx, stats): open stream, write encode_preamble(route_id, Some(peer), &[]) (replay empty), copy_bidirectional.

=== INTERNAL API (core -> website) ===
POST /api/internal/tunnel_up   (header x-internal-secret)
{ "tunnel_id":42, "subdomain":"duck-a1b2", "owner_id":7, "node_id":"edge-1", "public_host":"natforge.com",
  "routes":[ {"route_id":1,"route_type":"http","local_port":8000,"public_port":null},
             {"route_id":3,"route_type":"tcp","local_port":25565,"public_port":20007} ] }
POST /api/internal/tunnel_down { "tunnel_id":42 }      // releases tcp ports, SREM live:tunnels, DEL route keys
POST /api/internal/bandwidth   { "tunnel_id":42, "bytes_in":i64, "bytes_out":i64 }
GET  /api/internal/policy      -> { "blocked_ports":[u16], "blocked_regions":[String] }   // unchanged shape

=== INTERNAL API (website -> core) ===
POST {core_url}/internal/tunnels/{tunnel_id}/stop   // NEW, replaces subdomain-keyed mismatch; aborts ActiveTunnel + all RouteHandles

## SQL Schema

-- website_backend/migrations/0001_init.sql   (PostgreSQL 16)
-- Applied automatically at boot via sqlx::migrate!("./migrations"). Idempotent seeds.

CREATE TYPE route_type    AS ENUM ('http', 'https', 'tcp');
CREATE TYPE tunnel_status AS ENUM ('awaiting_agent', 'online', 'offline');
CREATE TYPE user_role     AS ENUM ('user', 'admin');

-- ---------------------------------------------------------------- users
-- id stays INT to match JWT sub: i32 (SessionClaims.sub / TunnelClaims.sub).
CREATE TABLE users (
    id            INT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    email         TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,                       -- argon2 PHC string
    role          user_role NOT NULL DEFAULT 'user',
    max_tunnels   INT NOT NULL DEFAULT 2,              -- per-user tunnel cap (Readme: up to 2)
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ---------------------------------------------------------------- tunnels
-- tunnel_id (id) is the durable PK embedded in the token; subdomain is the
-- unique live routing key (stable across reconnects).
CREATE TABLE tunnels (
    id          BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    subdomain   TEXT NOT NULL UNIQUE,
    owner_id    INT  NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    status      tunnel_status NOT NULL DEFAULT 'awaiting_agent',
    public_host TEXT NOT NULL,                          -- apex / node host shown to users
    node_id     TEXT,                                   -- which core node currently holds it (multi-node future)
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_seen   TIMESTAMPTZ                             -- mirror of Redis liveness
);
CREATE INDEX tunnels_owner_idx ON tunnels(owner_id);

-- ---------------------------------------------------------------- routes (1 tunnel : N routes)
-- http/https => public_port NULL (shared :80/:443, matched by subdomain).
-- tcp        => public_port set (dedicated pooled port, matched by port).
CREATE TABLE routes (
    id          BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    tunnel_id   BIGINT NOT NULL REFERENCES tunnels(id) ON DELETE CASCADE,
    route_id    SMALLINT NOT NULL,                      -- dense small id, unique within the tunnel/token
    kind        route_type NOT NULL,
    local_port  INT NOT NULL,
    public_port INT,
    UNIQUE (tunnel_id, route_id),
    UNIQUE (kind, public_port)                          -- one owner per dedicated tcp port (NULLs allowed many)
);
CREATE INDEX routes_tunnel_idx ON routes(tunnel_id);
-- Disallow two http (or two https) routes on the SAME subdomain (Host/SNI can't disambiguate):
CREATE UNIQUE INDEX routes_one_host_kind ON routes(tunnel_id, kind) WHERE kind IN ('http','https');

-- ---------------------------------------------------------------- port pool (allocator of record)
-- Seeded per node_id with the public TCP port range. Allocation:
--   WITH picked AS (SELECT port FROM port_pool WHERE node_id=$1 AND tunnel_id IS NULL
--                   ORDER BY port FOR UPDATE SKIP LOCKED LIMIT 1)
--   UPDATE port_pool p SET tunnel_id=$2, route_id=$3
--   FROM picked WHERE p.node_id=$1 AND p.port=picked.port RETURNING p.port;
-- Release on teardown: UPDATE port_pool SET tunnel_id=NULL, route_id=NULL WHERE tunnel_id=$1;
CREATE TABLE port_pool (
    node_id   TEXT NOT NULL,
    port      INT  NOT NULL,
    tunnel_id BIGINT REFERENCES tunnels(id) ON DELETE SET NULL,  -- NULL = free
    route_id  SMALLINT,
    PRIMARY KEY (node_id, port)
);

-- ---------------------------------------------------------------- bandwidth (periodic snapshot rows)
CREATE TABLE bandwidth_logs (
    id          BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    tunnel_id   BIGINT NOT NULL REFERENCES tunnels(id) ON DELETE CASCADE,
    owner_id    INT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    bytes_in    BIGINT NOT NULL DEFAULT 0,
    bytes_out   BIGINT NOT NULL DEFAULT 0,
    recorded_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX bw_owner_time_idx ON bandwidth_logs(owner_id, recorded_at DESC);
CREATE INDEX bw_tunnel_time_idx ON bandwidth_logs(tunnel_id, recorded_at DESC);

-- ---------------------------------------------------------------- ip hosts (edge nodes)
CREATE TABLE ip_hosts (
    user_id            INT PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    active             BOOLEAN NOT NULL DEFAULT false,
    max_bandwidth_mbps INT NOT NULL DEFAULT 100,
    geo_pref_only      BOOLEAN NOT NULL DEFAULT false,
    bytes_relayed      BIGINT NOT NULL DEFAULT 0,
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ---------------------------------------------------------------- admin policy
CREATE TABLE region_blocks (
    country_code CHAR(2) PRIMARY KEY,
    created_by   INT REFERENCES users(id),
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE TABLE port_blocks (
    port       INT PRIMARY KEY,
    created_by INT REFERENCES users(id),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ---------------------------------------------------------------- reserved subdomains (anti-shadow)
CREATE TABLE reserved_subdomains ( name TEXT PRIMARY KEY );

-- ---------------------------------------------------------------- seeds (idempotent)
INSERT INTO port_blocks(port) VALUES (25),(465),(587) ON CONFLICT DO NOTHING;     -- thesis §7 SMTP bans
INSERT INTO region_blocks(country_code) VALUES ('RU') ON CONFLICT DO NOTHING;     -- current main.rs seed
INSERT INTO reserved_subdomains(name)
  VALUES ('www'),('api'),('admin'),('device'),('app'),('edge') ON CONFLICT DO NOTHING;

-- NOTE: device codes are Redis-only (TTL); no device_codes table (audit table optional, omitted).
-- NOTE: port_pool is seeded at runtime per node on first boot (see redis_keys / startup), NOT here,
--       because the range + node_id come from core_proxy config, not the website migration.

## Redis Keys

All keys prefixed `nf:`. Connections via redis 0.27 ConnectionManager (tokio-comp).

DEVICE FLOW (website owns; replaces in-memory device_codes map; RFC 8628):
  nf:devcode:{user_code}        HASH { device_code, approved_user ("" until approved | "<uid>"), created_at }
                                SETEX 600   (expires_in = 10 min)
  nf:devcode:dc:{device_code}   STRING -> {user_code}     (reverse index for /device/token poll)
                                SETEX 600   (same TTL so both expire together)
  device_approve: HSET nf:devcode:{user_code} approved_user <uid>  (TTL preserved via KEEPTTL or re-EXPIRE).

LIVE ROUTING MIRROR (core owns; written on tunnel_up, refreshed by 10s heartbeat, TTL 30s):
  nf:route:host:{subdomain}     HASH { node_id, tunnel_id, route_id, mode (http|https), local_port }   EX 30
  nf:route:port:{public_port}   HASH { node_id, tunnel_id, route_id, subdomain, local_port }           EX 30
  nf:tunnel:live:{subdomain}    STRING "1"                                                             EX 30
  nf:node:{node_id}             HASH { addr, http_port, https_port, last_seen }                        EX 30
  Liveness = key existence. On core crash the keys self-expire; the website reconciliation sweep
  (on tunnel_down + periodic) returns ports to the pool for tunnels whose live key is gone.
  v1 core uses its in-process state.routes / state.port_routes as the hot-path source of truth and
  ONLY mirrors to Redis (no cross-node read path yet); multi-node read is the documented next step.

PORT POOL FREE-SET (optional fast cache; Postgres port_pool is authoritative):
  nf:portpool:{node_id}:free    SET of free ports   (mirror; SPOP/SADD). Authoritative alloc is the
                                Postgres FOR UPDATE SKIP LOCKED query — Redis set is advisory only.

RATE / DDoS (core; cross-node shared layer on top of in-process sliding window):
  nf:rate:{ip}                  STRING counter, INCR then EXPIRE 1   (1s window, cross-node)
  nf:blacklist                  SET of ip            (no TTL; admin + auto-trip). Core checks this set
                                in addition to the in-process DdosProtector blacklist.

POLICY SNAPSHOT (website writes on admin change; core may read instead of polling /policy):
  nf:policy:blocked_ports       SET of port
  nf:policy:blocked_regions     SET of country_code
  (v1 keeps the existing 30s HTTP /api/internal/policy poll; Redis policy keys are an optimization.)

PUB/SUB (future multi-node + live dashboard; written in v1, consumed later):
  channel nf:events   JSON { kind: "tunnel_up"|"tunnel_down"|"policy_changed", subdomain, tunnel_id, node_id, ts }

TTL SUMMARY: device codes 600s; route/live/node keys 30s with 10s heartbeat refresh; rate 1s; blacklist + policy + pool: no TTL.

## Crate Changes

### natforge-proto (NEW workspace member)

Create natforge-proto/Cargo.toml (deps: serde+derive, tokio io util, anyhow) and src/lib.rs. Holds the SHARED wire contract so core and agent never drift: RouteMode enum (Http|Https|Tcp, snake_case serde); handshake structs AgentHello{tunnel_token, role, routes:Vec<AgentRouteBinding{route_id:u16,local_port:u16}>}, CoreReply{Ok{tunnel_id:i64,subdomain,routes:Vec<RouteResult{route_id,mode,public_endpoint}>} | Error{message}}; STREAM_MAGIC=b"NFS1"; encode_preamble(route_id, Option<SocketAddr>, &[u8])->Vec<u8> and async read_preamble<R:AsyncRead+Unpin>(&mut R)->(u16,Option<SocketAddr>,Vec<u8>). Add to root Cargo.toml [workspace] members. Unit tests for preamble round-trip.

### website_backend

Cargo.toml already has sqlx/redis/dotenvy (verified) — add natforge-proto path dep. config.rs: add database_url, redis_url. db/connection.rs: rewrite to Db{pg:PgPool, redis:ConnectionManager} + AppState{config,http,db} + AppState::connect(config) (fail-fast Postgres with anyhow context, sqlx::migrate!, Redis ConnectionManager). NEW db/mod.rs declaring connection+queries+redis_ops. NEW db/queries.rs (runtime sqlx::query/query_as — NO compile-time macros, to keep cargo build DB-free): create_user(CASE first-admin), user_by_email/by_id/count, reserve_tunnel(subdomain ON CONFLICT retry), add_route, alloc_tcp_port(FOR UPDATE SKIP LOCKED), release_ports_for_tunnel, tunnels_for_owner(JOIN routes), tunnel_owned_count, set_tunnel_online, delete_tunnel(by id, owner/admin), all_tunnels, append_bandwidth, ip_host_* , region/port_blocks CRUD, active_edge_count. NEW db/redis_ops.rs: devcode_put/approve/by_device, live_subdomains, port-pool mirror. main.rs: AppState::connect(config).await?, delete in-code seeding (now in migration). models/user.rs: keep struct shapes; widen byte counters to i64; add Route + TunnelView shapes used by tunnels.rs. jwt.rs: new multi-route TunnelClaims+RouteClaim+RouteMode, issue_tunnel_token(secret,user_id,tunnel_id,subdomain,routes). handlers/auth.rs: users+device codes via Db/Redis (first-user-admin via SQL). handlers/tunnels.rs: request_tunnel(RequestTunnelReq{routes}) allocates subdomain+ports+persists+mints one token; get_tunnels->Vec<TunnelView with nested routes>; stop_tunnel(Path<i64> tunnel_id) posts to core /internal/tunnels/{tunnel_id}/stop. handlers/internal.rs: tunnel_up{tunnel_id,routes[],node_id}, tunnel_down{tunnel_id} releases ports, bandwidth{tunnel_id} -> bandwidth_logs. handlers/iphost.rs + admin.rs: SQL-backed. routes/mod.rs: change DELETE /api/tunnels/{subdomain} to /api/tunnels/{tunnel_id}. (auth_routes.rs already deleted per git status — fine.)

### core_proxy_backend

Cargo.toml: add redis 0.27 (tokio-comp, connection-manager), dotenvy 0.15, natforge-proto path dep. (NO sqlx — website is the only Postgres writer.) config.rs: add redis_url, node_id, http_port(80/dev8080), https_port(443/dev8443), max_header_bytes(16384). jwt.rs: mirror multi-route TunnelClaims/RouteClaim/RouteMode; verify_tunnel_token enforces v==1 + per-route structural validity. state.rs: OpenStream gains route_id:u16; add RouteHandle{route_id,mode,open_tx,stats}; CoreState gains routes:RwLock<HashMap<String,RouteHandle>> (subdomain key) + port_routes:RwLock<HashMap<u16,RouteHandle>> (port key) + redis:ConnectionManager; ActiveTunnel gains tunnel_id,route_ids,public_ports,listener_handles:Vec<JoinHandle>; REMOVE free_ports/alloc_port/release_port; CoreState::connect(config).await is now async (Redis). tunnel/mod.rs: mirror new AgentHello/CoreReply via natforge-proto; rewrite handle_agent to reconcile claims<->bindings by route_id, BIND each signed tcp public_port (not alloc), register http/https RouteHandles into state.routes + spawn dedicated tcp listeners into state.port_routes, ONE driver (unchanged mux::run_client_driver), bridge_public_connection writes encode_preamble(route_id,peer,&[]) before copy, teardown removes all registry entries + aborts all listener_handles; add `pub mod shared;`. tunnel/shared.rs (NEW): run_http/run_https acceptors (per-conn 5s read-timeout spawn), hand-rolled Host scan + bounds-checked SNI parser (passthrough, no termination), route_and_splice(peek->resolve state.routes->open stream->preamble carries peeked replay->copy). tunnel/mux.rs: UNCHANGED in shape (confirms core stays yamux Client; OpenStream now carries route_id but driver just forwards reply). main.rs: connect Redis (fail-fast), spawn run_http+run_https alongside run_control_plane; CoreState::new -> CoreState::connect(config).await?. reporter.rs: tunnel_up carries routes[]+node_id; write/refresh/del Redis nf:route:*/nf:tunnel:live keys; add 10s heartbeat EXPIRE task; report_bandwidth keyed by tunnel_id. api/routes.rs: add POST /internal/tunnels/{tunnel_id}/stop (abort ActiveTunnel + all RouteHandles); update list_tunnels TunnelView for multi-route. ddos/filter.rs: add nf:blacklist Redis cross-node check on top of in-process window (optional, Phase 5).

### proxy_node

Cargo.toml: add natforge-proto path dep. protocol.rs: re-export natforge-proto AgentHello/CoreReply/RouteMode/encode_preamble/read_preamble; keep read_frame/write_frame helpers (unchanged framing). service_host.rs: Reservation now {tunnel_id, subdomain, full_host, tunnel_token, routes:Vec<{route_id,mode,local_port}>}; reserve() POSTs the requested routes; run() takes Vec<RouteSpec>; build routes:Arc<HashMap<u16,u16>> (route_id->local_port) once, clone Arc into each spawn; rewrite handle_stream to read_preamble first, map route_id->local_port, dial, write replay, copy_bidirectional; print every RouteResult.public_endpoint on Ok. main.rs: add repeatable --route <local_port>:<mode> (clap Vec<String>, parse split(':')); keep legacy --local-port as one tcp route for back-compat; pass parsed routes into service_host::run.

### frontend (static, not a crate)

api/client.js: requestTunnel(routes) sends JSON body; stopTunnel(tunnelId) hits /tunnels/{id}. views/dashboard.html: getTunnels() now returns tunnels with nested routes — render one sub-row per route (http/https => clickable https://sub.domain, tcp => host:port for Minecraft 'Add Server'); request modal gains a small route builder (local_port + mode dropdown) and prints the agent command with --route flags; stop button passes tunnel_id not subdomain (currently passes t.subdomain at dashboard.html:113).

### install.sh + docker-compose + .env

docker-compose.yml: keep as-is (already correct). .env (gitignored): DATABASE_URL, REDIS_URL, JWT_SECRET, INTERNAL_SECRET, NATFORGE_DOMAIN, NODE_ID, PUBLIC_HOST, PUBLIC_PORT_MIN/MAX, dev HTTP_PORT=8080/HTTPS_PORT=8443. install.sh: add DATABASE_URL/REDIS_URL to website unit; add REDIS_URL/NODE_ID/HTTP_PORT/HTTPS_PORT to core unit; add AmbientCapabilities=CAP_NET_BIND_SERVICE to the core systemd unit so it can bind :80/:443 in prod (NoNewPrivileges currently true — must add AmbientCapabilities + CapabilityBoundingSet=CAP_NET_BIND_SERVICE).


## Test Plan

PREP (no real DNS anywhere):
  docker compose up -d
  # website: connects Postgres+Redis, runs migrations, seeds reserved/ports/regions
  DATABASE_URL=postgres://natforge:natforge@127.0.0.1:5432/natforge \
    REDIS_URL=redis://127.0.0.1:6379 cargo run -p website_backend &        # :3000
  # core: binds control :4000, internal :3001, shared :8080 (http) :8443 (https), tcp pool 20000-20100
  PUBLIC_HOST=natforge.com NODE_ID=edge-1 REDIS_URL=redis://127.0.0.1:6379 \
    HTTP_PORT=8080 HTTPS_PORT=8443 PUBLIC_PORT_MIN=20000 PUBLIC_PORT_MAX=20100 \
    cargo run -p core_proxy_backend &
  # origins (stand-ins for the user's real services):
  python3 -m http.server 8000 &                                           # HTTP origin
  openssl req -x509 -newkey rsa:2048 -keyout k.pem -out c.pem -days 1 -nodes \
    -subj '/CN=duck-a1b2.natforge.com'
  openssl s_server -accept 8443 -www -cert c.pem -key k.pem &             # NOTE: pick a free port; if 8443
                                                                          #   clashes with HTTPS_PORT use 9443
                                                                          #   and route 9443:tcp? No — TLS origin
                                                                          #   is the agent's LOCAL port; use 9443.
  ( while true; do printf 'MC-OK\n' | nc -l 25565; done ) &               # raw TCP origin (Minecraft stand-in)

  # register + reserve a 3-route tunnel:
  TOKEN=$(curl -s :3000/api/auth/register -d '{"email":"a@b.c","password":"secret1"}' | jq -r .token)
  RSV=$(curl -s :3000/api/tunnels/request -H "Authorization: Bearer $TOKEN" \
    -d '{"routes":[{"mode":"http","local_port":8000},{"mode":"https","local_port":9443},{"mode":"tcp","local_port":25565}]}')
  echo "$RSV" | jq .            # note .subdomain (e.g. duck-a1b2), .tunnel_token, tcp public_endpoint (e.g. :20007)
  SUB=$(echo "$RSV" | jq -r .subdomain); TT=$(echo "$RSV" | jq -r .tunnel_token)
  TCP_PORT=$(echo "$RSV" | jq -r '.routes[] | select(.mode=="tcp") | .public_endpoint' | sed 's/.*://')
  # launch agent with all three routes over ONE session:
  cargo run -p proxy_node -- service-host --token "$TT" \
    --route 8000:http --route 9443:https --route 25565:tcp &

(1) HTTP friend via SUBDOMAIN on shared :80 (Host header, no DNS):
  curl -s -H "Host: $SUB.natforge.com" http://127.0.0.1:8080/
  # EXPECT: python directory listing (origin reached purely by subdomain demux).
  curl -i -H 'Host: nope.natforge.com' http://127.0.0.1:8080/    # EXPECT: 404/400, no stream opened.

(2) HTTPS via SNI passthrough on shared :443 (core never decrypts):
  curl -vk --resolve "$SUB.natforge.com:8443:127.0.0.1" "https://$SUB.natforge.com:8443/"
  # EXPECT: TLS handshake completes against the ORIGIN cert CN=duck-a1b2... (proves passthrough; core only peeked SNI).
  openssl s_client -connect 127.0.0.1:8443 -servername nope.natforge.com </dev/null   # EXPECT: connection closed/reset.

(3) Raw TCP via DEDICATED port (no hostname):
  printf '' | nc 127.0.0.1 "$TCP_PORT"     # EXPECT: "MC-OK" relayed over yamux to 127.0.0.1:25565 (preamble replay_len=0).

(4) MULTIPLE simultaneous users sharing :80 (scale + collision-free allocation):
  T2=$(curl -s :3000/api/auth/register -d '{"email":"x@y.z","password":"secret2"}' | jq -r .token)
  RSV2=$(curl -s :3000/api/tunnels/request -H "Authorization: Bearer $T2" -d '{"routes":[{"mode":"http","local_port":8000}]}')
  SUB2=$(echo "$RSV2" | jq -r .subdomain); TT2=$(echo "$RSV2" | jq -r .tunnel_token)
  cargo run -p proxy_node -- service-host --token "$TT2" --route 8000:http &
  curl -s -H "Host: $SUB.natforge.com"  http://127.0.0.1:8080/    # user1 still works
  curl -s -H "Host: $SUB2.natforge.com" http://127.0.0.1:8080/    # user2 works on SAME shared :80 (different subdomain)
  # EXPECT: both 200; demonstrates unlimited-user sharing of one port. Subdomains differ (tunnels UNIQUE PK).

(5) MULTI-PORT / multi-route SINGLE session (target B):
  curl -s -H "Host: $SUB.natforge.com" http://127.0.0.1:8080/ && printf '' | nc 127.0.0.1 "$TCP_PORT"
  # EXPECT: both succeed over the SAME yamux session (route_id 1 vs route_id 3 selected by preamble).

(6) STATE SURVIVES RESTART (durability, target D):
  curl -s :3000/api/tunnels -H "Authorization: Bearer $TOKEN" | jq '.[0] | {subdomain, routes}'   # note SUB + TCP_PORT
  kill %1 %2 || true ; pkill -f website_backend ; pkill -f core_proxy_backend     # crash both planes
  DATABASE_URL=... REDIS_URL=... cargo run -p website_backend &                   # reloads tunnels/routes/port_pool from Postgres
  PUBLIC_HOST=natforge.com NODE_ID=edge-1 ... cargo run -p core_proxy_backend &   # reclaims its port_pool rows
  # agent reconnect loop re-handshakes automatically (re-reserves -> SAME subdomain+tunnel_id+ports from durable rows):
  curl -s :3000/api/tunnels -H "Authorization: Bearer $TOKEN" | jq '.[0] | {subdomain, routes}'   # EXPECT: SAME subdomain + SAME tcp port
  curl -s -H "Host: $SUB.natforge.com" http://127.0.0.1:8080/                     # EXPECT: works again

UNIT TESTS (cargo test):
  - natforge-proto: round-trip encode_preamble/read_preamble (IPv4, IPv6, none, replay 0 and N bytes).
  - shared.rs parse_sni: table of captured ClientHello byte vectors (curl --resolve + rustls-generated), plus
    truncated/no-SNI/non-TLS inputs -> NeedMore/NoSni/NotTls (fail-closed).
  - shared.rs host scan: split-across-reads Host header, missing Host, Host with :port and trailing dot.

OPTIONAL browser test (real browser, not just curl):
  echo "127.0.0.1 $SUB.natforge.com $SUB2.natforge.com" | sudo tee -a /etc/hosts
  # then visit http://duck-a1b2.natforge.com:8080 — hits the shared edge by Host header.

## Open Questions

- Per-user route cap policy: Readme says 'up to 2 public ports' and current code MAX_TUNNELS_PER_USER=2. Proposed: per tunnel = at most 1 http + 1 https + up to 2 tcp; tunnels per user still <=2. Confirm whether http/https should also count against a limit (they consume no port) or be unlimited per tunnel.
- Token TTL is 10 min and tcp ports are baked into the token. On reconnect after expiry the agent re-reserves and MUST get the same subdomain+ports from the durable row — confirm reserve_tunnel reuses an existing 'awaiting_agent'/'offline' row for the user rather than allocating new ports (otherwise the port leaks until the reconciliation sweep).
- Does the HTTPS origin in local testing present its own cert (passthrough) — yes per design — but the test recipe needs the agent's LOCAL TLS port (e.g. 9443) to differ from the core's shared HTTPS_PORT (8443). Confirm we standardize local TLS origin on 9443 to avoid the collision noted in test_plan.
- Multi-node cross-edge forwarding is written to Redis (node_id, node addr) but NOT dialed in v1. Confirm single-core-node is acceptable for the thesis demo (Scenario A/B are single-node) and cross-node is documented future work.
- bandwidth_logs are periodic snapshot rows (append every 5s reporter tick). Confirm the dashboard should show the latest row (current total) vs a SUM — current internal::bandwidth overwrites; new design appends. Need a 'latest snapshot per tunnel' query for get_tunnels/admin stats.
- Should the core read policy (blocked_ports/regions) from Redis nf:policy:* or keep the existing 30s HTTP /api/internal/policy poll? Plan keeps the HTTP poll for v1; Redis policy is an optimization. Confirm.
- Subdomain-only tunnels (tcp-only) have subdomain='' in claims but the tunnels table requires subdomain UNIQUE NOT NULL. Decision needed: always allocate a subdomain even for tcp-only tunnels (simplest, recommended) OR allow NULL subdomain and key the ActiveTunnel by 'tid:<id>'. Plan assumes a subdomain is always allocated.

## Full Plan

# NatForge "Complete" — Unified Implementation Plan

This plan keeps everything that works today: the two-plane (control/data) split, the stateless 10-minute tunnel token, core = yamux **Client** (opens 1 outbound stream per inbound public conn), agent = yamux **Server** (accepts streams, `copy_bidirectional` to `127.0.0.1:local_port`), the poll-based `run_client_driver` state machine in `core_proxy_backend/src/tunnel/mux.rs`, the Argon2+JWT auth, the RFC8628 device flow, the DDoS heuristic, the mock Cloudflare DNS, and the simulated WireGuard struct.

## Decisive conflict resolutions (read first)

1. **Per-stream demux wire format**: adopt expert-1's magic-framed binary `StreamPreamble` (`NFS1`) for EVERY core->agent stream. It subsumes the rival 4-byte / 2-byte route_id schemes AND solves the :80/:443 peek-replay problem in one frame (route_id + original peer + replayed bytes). Reject the bare "2/4-byte route_id with no magic" because the peeked HTTP/TLS bytes must travel WITH the route selector; a magic+version header also lets us reject version drift cleanly.
2. **Routing key vs durable PK**: subdomain is the live routing key (unique, `UNIQUE` constraint); `tunnel_id BIGINT` (DB identity) is the durable PK and is embedded in the token so reconnects keep the subdomain. (Reconciles expert-3 `tunnel_id` with expert-2/expert-5 subdomain-keyed lookup.)
3. **Port allocation authority**: the **website** is the allocator of record via a Postgres `port_pool` table with `SELECT ... FOR UPDATE SKIP LOCKED`. The allocated `public_port` is baked into the signed token. The core **binds exactly the signed port** and no longer calls `alloc_port()`. (Reconciles experts 3/4/5; retires `CoreState::free_ports`/`alloc_port`/`release_port`.)
4. **Route model**: a session has 1..=N routes. `http` and `https` routes share ONE subdomain (Host header on :80 / SNI on :443, no port cost); `tcp` routes each consume one dedicated pooled port. ONE token authorizes the whole route set; ONE yamux session multiplexes all of them.
5. **HTTP parsing**: hand-rolled minimal Host scan (NOT `httparse`) to avoid adding a direct dep and to keep the read-loop identical in shape to the SNI path. (Rejects expert-2's httparse; both are fine but hand-rolled keeps zero new parse deps and one code style.)
6. **TLS**: SNI passthrough only (L4), hand-rolled bounds-checked ClientHello parser, no termination. (All experts agree.)
7. **Core data layer**: core gets a Redis `ConnectionManager` (routing mirror, port-pool is owned by website but core reads liveness, rate counters) but **keeps the in-process `RwLock<HashMap>` of yamux channels as hot-path source of truth** (yamux `mpsc::Sender` is not serializable). Core does NOT get a `PgPool` — it persists through the existing `reporter` HTTP path to the website, which owns Postgres. (Simplifies expert-4: one writer to Postgres = the website.)
8. **Device codes / liveness / rate / port-pool-free-set**: Redis. **Users/tunnels/routes/bandwidth/blocks/port_pool rows**: Postgres.
9. **Cross-node forwarding** (expert-5): documented as future work, NOT implemented in v1. v1 is single-core-node; Redis mirror + `node_id` columns are written so multi-node is a later additive step.

---

## ORDERING — each step compiles before the next

### Phase 0 — Shared protocol crate (foundation, no behavior change)
**Step 0.1** Create new workspace member `natforge-proto/` (one file). It holds the wire types and codec shared by core + agent so they can never drift. Add to root `Cargo.toml` members. This compiles standalone.

### Phase 1 — Database + ephemeral store (website only; core unchanged, still compiles)
**Step 1.1** `website_backend/migrations/0001_init.sql` (full DDL below). 
**Step 1.2** Rewrite `website_backend/src/db/connection.rs`: `Db { pg: PgPool, redis: ConnectionManager }`, `AppState::connect(config)` (fail-fast on Postgres, run `sqlx::migrate!`, connect Redis). Add `database_url`/`redis_url` to `website_backend/src/config.rs`. Use **runtime** `sqlx::query`/`query_as` (NOT the `query!` macros) so `cargo build` never needs a live DB or committed `.sqlx` cache — this is the decisive choice to keep the build friction-free.
**Step 1.3** New `website_backend/src/db/queries.rs` (Postgres helpers) and `website_backend/src/db/redis_ops.rs` (device codes, port-pool, liveness, rate). Add `pub mod queries; pub mod redis_ops;` to `db/mod.rs` (create if absent — currently `db` only has `connection`).
**Step 1.4** Rewrite handler bodies to call the DB layer instead of the in-memory maps, keeping the SAME response JSON shapes for handlers that the frontend already consumes: `auth.rs` (users + device codes), `iphost.rs` (`ip_hosts` table), `admin.rs` (blocks + stats via SQL). First-user-is-admin via single SQL `CASE` (see edge cases). `main.rs`: `AppState::connect(config).await?`, remove the in-code port/region seeding block (seeded by migration).
At end of Phase 1 the website is fully Postgres/Redis-backed; core still talks to it over the unchanged internal API. **Compiles and runs** with `docker compose up -d`.

### Phase 2 — Multi-route token + reservation (website), backward-compatible core
**Step 2.1** `website_backend/src/jwt.rs`: replace single-route `TunnelClaims` with the multi-route version (`v`, `tunnel_id`, `subdomain`, `routes: Vec<RouteClaim>`). Add `issue_tunnel_token(secret, user_id, tunnel_id, subdomain, routes)`. 
**Step 2.2** `website_backend/src/handlers/tunnels.rs`: `request_tunnel` now takes `RequestTunnelReq { routes: Vec<RequestedRoute{ mode, local_port }> }`, allocates a stable subdomain (Postgres `INSERT ... ON CONFLICT DO NOTHING RETURNING`, retry 5x), allocates a `tcp` port per tcp-route via `port_pool` `FOR UPDATE SKIP LOCKED`, persists `tunnels` + `routes`, mints ONE token, returns `TunnelRequestRes { tunnel_id, subdomain, full_host, tunnel_token, routes, status }`. `get_tunnels` returns `Vec<TunnelView>` with nested `routes`. `stop_tunnel` keyed by `tunnel_id`. Reject blocked local ports up front. Frontend `requestTunnel()` and `dashboard.html` updated (Phase 6).
**Step 2.3** `website_backend/src/handlers/internal.rs`: `tunnel_up` accepts `routes: Vec<{route_id, route_type, local_port, public_port?}>` + `node_id`; loops and upserts. `tunnel_down` releases tcp ports back to `port_pool` and `live:tunnels`. `bandwidth` writes `bandwidth_logs`. **Fix the existing route mismatch**: website `stop_tunnel` posts to `{core_url}/internal/tunnels/{tunnel_id}/stop`; add the matching core route in Step 4.
At end of Phase 2 the website mints multi-route tokens. The OLD single-route core still verifies its OWN copy of `TunnelClaims` — so until Phase 3/4 ship together, do not point the new agent at the old core. (They ship as one release; see risks.)

### Phase 3 — Agent multi-route + preamble (proxy_node)
**Step 3.1** `proxy_node/src/protocol.rs`: re-export `natforge-proto` types; widen `AgentHello` to `{ tunnel_token, role, routes: Vec<AgentRouteBinding{route_id, local_port}> }`; widen `CoreReply::Ok` to `{ tunnel_id, subdomain, routes: Vec<RouteResult> }`.
**Step 3.2** `proxy_node/src/service_host.rs`: `reserve()` sends the requested routes and parses the new `Reservation { tunnel_id, subdomain, full_host, tunnel_token, routes }`; build `routes: Arc<HashMap<u16,u16>>` (route_id->local_port); `run(...)` takes `Vec<RouteSpec>`; `handle_stream` reads the preamble first (`natforge_proto::read_preamble`), looks up local_port, dials, replays, splices.
**Step 3.3** `proxy_node/src/main.rs`: `--route <local_port>:<mode>` repeatable (clap `Vec<String>`), parse "8000:http"; keep legacy `--local-port` as sugar for one `tcp` route.

### Phase 4 — Core multi-route bind + registries (core_proxy_backend)
**Step 4.1** `core_proxy_backend/Cargo.toml`: add `redis = { version = "0.27", features = ["tokio-comp","connection-manager"] }`, `dotenvy = "0.15"`. Add `natforge-proto` path dep. (No sqlx in core.)
**Step 4.2** `core_proxy_backend/src/config.rs`: add `redis_url`, `node_id`, `http_port` (default 80 / dev 8080), `https_port` (default 443 / dev 8443), `max_header_bytes` (16384).
**Step 4.3** `core_proxy_backend/src/jwt.rs`: mirror the new multi-route `TunnelClaims` + `RouteClaim` + `RouteMode`; `verify_tunnel_token` enforces `v==1`, `purpose=="tunnel"`, structural route validity.
**Step 4.4** `core_proxy_backend/src/state.rs`: add `routes: RwLock<HashMap<String, RouteHandle>>` (key=subdomain, for http/https), `port_routes: RwLock<HashMap<u16, RouteHandle>>` (key=public_port, for tcp), `redis: ConnectionManager`; `OpenStream` gains `route_id: u16`; `ActiveTunnel` gains `tunnel_id, route_ids, public_ports, listener_handles`. Remove `free_ports`/`alloc_port`/`release_port`.
**Step 4.5** `core_proxy_backend/src/tunnel/mod.rs`: mirror new `AgentHello`/`CoreReply` (Deserialize/Serialize sides); rewrite `handle_agent` to reconcile claims↔bindings, bind each signed tcp port, register http/https into `routes`, register tcp into `port_routes`, spawn one driver, write the preamble in `bridge_*`, tear down all routes. Add `pub mod shared;`.
**Step 4.6** `core_proxy_backend/src/tunnel/shared.rs` (NEW): `run_http`, `run_https`, the hand-rolled Host + SNI parsers, `route_and_splice` (peek → resolve subdomain in `state.routes` → open stream → preamble(replay) → copy).
**Step 4.7** `core_proxy_backend/src/main.rs`: connect Redis (fail-fast), spawn `tunnel::shared::run_http` + `run_https`. 
**Step 4.8** `core_proxy_backend/src/reporter.rs`: `tunnel_up` carries `routes[]` + `node_id`; on up/down write/del Redis route+liveness keys; add a 10s heartbeat task that `EXPIRE`s route/live keys to 30s.
**Step 4.9** `core_proxy_backend/src/api/routes.rs`: add `POST /internal/tunnels/{tunnel_id}/stop` (matches website), keep `/internal/tunnels/{subdomain}` for compat or migrate.
At end of Phase 4 the full data path works: subdomain on :80/:443 + dedicated tcp ports, multi-route, one session.

### Phase 5 — DDoS Redis backing (optional hardening)
**Step 5.1** `core_proxy_backend/src/ddos/filter.rs`: keep the in-process sliding window as a fast pre-filter; add a cross-node `nf:blacklist` Redis set check. (Additive; can ship later.)

### Phase 6 — Frontend
**Step 6.1** `frontend/api/client.js`: `requestTunnel(routes)` sends a body; `stopTunnel(tunnelId)`. 
**Step 6.2** `frontend/views/dashboard.html`: render one sub-row per route (HTTP/HTTPS show clickable `https://sub.domain`, TCP shows `host:port`); add a small route builder in the request modal; print the agent command with `--route` flags.

### Phase 7 — Docker / env / install
**Step 7.1** Keep existing `docker-compose.yml` (already correct: postgres:16-alpine + redis:7-alpine + healthchecks + named volume). 
**Step 7.2** Add `.env` (gitignored already) with `DATABASE_URL`, `REDIS_URL`, `JWT_SECRET`, `INTERNAL_SECRET`, `NATFORGE_DOMAIN`, `NODE_ID`, dev `HTTP_PORT=8080`/`HTTPS_PORT=8443`. 
**Step 7.3** `install.sh`: add `DATABASE_URL`/`REDIS_URL`/`NODE_ID`/`HTTP_PORT`/`HTTPS_PORT` env to the core + website units and `AmbientCapabilities=CAP_NET_BIND_SERVICE` for the core unit (so it can bind :80/:443 in prod).

---

## Detailed deltas (signatures, struct/field names)

### `natforge-proto/src/lib.rs` (NEW crate)
```rust
use serde::{Deserialize, Serialize};
use std::net::{IpAddr, SocketAddr};
use tokio::io::{AsyncRead, AsyncReadExt};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteMode { Http, Https, Tcp }

// ---- handshake ----
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRouteBinding { pub route_id: u16, pub local_port: u16 }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentHello {
    pub tunnel_token: String,
    pub role: String,                       // "service_host"
    pub routes: Vec<AgentRouteBinding>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum CoreReply {
    Ok { tunnel_id: i64, subdomain: String, routes: Vec<RouteResult> },
    Error { message: String },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteResult { pub route_id: u16, pub mode: RouteMode, pub public_endpoint: String }

// ---- per-stream binary preamble (see wire_protocol) ----
pub const STREAM_MAGIC: &[u8; 4] = b"NFS1";
pub fn encode_preamble(route_id: u16, peer: Option<SocketAddr>, replay: &[u8]) -> Vec<u8> { /* see wire_protocol */ }
pub async fn read_preamble<R: AsyncRead + Unpin>(r: &mut R)
    -> anyhow::Result<(u16, Option<SocketAddr>, Vec<u8>)> { /* see wire_protocol */ }
```

### `website_backend/src/jwt.rs` (and mirrored in `core_proxy_backend/src/jwt.rs`)
```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteClaim {
    pub route_id: u16,
    pub mode: natforge_proto::RouteMode,   // http|https|tcp
    pub host: Option<String>,              // Some for http/https = "sub.natforge.com"; None for tcp
    pub public_port: Option<u16>,          // Some for tcp (pool port); None for http/https
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TunnelClaims {
    pub v: u8,                  // == 1
    pub sub: i32,               // owner user id (unchanged name)
    pub tunnel_id: i64,         // durable PK
    pub subdomain: String,      // "" if tcp-only
    pub purpose: String,        // "tunnel"
    pub routes: Vec<RouteClaim>,
    pub exp: usize,
}
pub fn issue_tunnel_token(secret:&str, user_id:i32, tunnel_id:i64, subdomain:&str, routes:Vec<RouteClaim>) -> String;
```
`core_proxy_backend/src/jwt.rs::verify_tunnel_token` additionally checks `claims.v == 1` and per-route: http/https ⇒ `host.is_some() && public_port.is_none()`; tcp ⇒ `public_port.is_some() && host.is_none()`; route_ids unique.

### `website_backend/src/handlers/tunnels.rs`
```rust
#[derive(Deserialize)] pub struct RequestedRoute { pub mode: RouteMode, pub local_port: u16 }
#[derive(Deserialize)] pub struct RequestTunnelReq { pub routes: Vec<RequestedRoute> }
#[derive(Serialize)] pub struct ReservedRoute { pub route_id:u16, pub mode:String, pub local_port:u16, pub public_endpoint:String }
#[derive(Serialize)] pub struct TunnelRequestRes { pub tunnel_id:i64, pub subdomain:String, pub full_host:String, pub tunnel_token:String, pub routes:Vec<ReservedRoute>, pub status:String }
pub async fn request_tunnel(State(state):State<SharedState>, user:AuthUser, Json(req):Json<RequestTunnelReq>) -> Result<Json<TunnelRequestRes>,(StatusCode,String)>;

#[derive(Serialize)] pub struct RouteView { pub route_id:u16, pub mode:String, pub local_port:i32, pub public_endpoint:String, pub status:String }
#[derive(Serialize)] pub struct TunnelView { pub tunnel_id:i64, pub subdomain:String, pub full_host:String, pub public_host:String, pub status:String, pub bytes_in:i64, pub bytes_out:i64, pub created_at:chrono::DateTime<chrono::Utc>, pub routes:Vec<RouteView> }
pub async fn get_tunnels(State<SharedState>, AuthUser) -> Json<Vec<TunnelView>>;
pub async fn stop_tunnel(State<SharedState>, AuthUser, Path(tunnel_id):Path<i64>) -> Result<Json<serde_json::Value>,(StatusCode,String)>;
```
Validation: enforce `routes.len()` policy (cap = 1 http + 1 https + up to 2 tcp per tunnel; total tunnels per user still ≤ `MAX_TUNNELS_PER_USER=2`); reject duplicate `http`/`https` modes; reject blocked local ports; on any failure after popping ports, `SADD` them back.

### `core_proxy_backend/src/state.rs`
```rust
pub struct OpenStream { pub route_id: u16, pub reply: oneshot::Sender<Result<Stream, ConnectionError>> }
#[derive(Clone)]
pub struct RouteHandle { pub route_id:u16, pub mode:RouteMode, pub open_tx:mpsc::Sender<OpenStream>, pub stats:Arc<TunnelStats> }
pub struct ActiveTunnel {
    pub tunnel_id:i64, pub subdomain:String, pub owner_id:i32,
    pub route_ids:Vec<u16>, pub public_ports:Vec<u16>,
    pub open_tx:mpsc::Sender<OpenStream>, pub stats:Arc<TunnelStats>,
    pub listener_handles:Vec<tokio::task::JoinHandle<()>>,
}
pub struct CoreState {
    pub config:Config,
    pub tunnels:RwLock<HashMap<String,ActiveTunnel>>,   // key = subdomain (tcp-only => key = "tid:<id>")
    pub routes:RwLock<HashMap<String,RouteHandle>>,      // key = subdomain (http/https)
    pub port_routes:RwLock<HashMap<u16,RouteHandle>>,    // key = public_port (tcp)
    pub redis:redis::aio::ConnectionManager,
    pub ddos:DdosProtector,
    pub blocked_ports:RwLock<Vec<u16>>,
    pub http:reqwest::Client,
}
```
`bridge_public_connection` keeps its shape but takes `route_id` and writes `encode_preamble(route_id, Some(peer), &[])` to the compat stream before `copy_bidirectional`.

### `core_proxy_backend/src/tunnel/shared.rs` (NEW)
`run_http(state)` binds `state.config.http_port`; per-conn spawn `serve_http` (5s read timeout): loop-read into `Vec<u8>` (cap `max_header_bytes`), scan for case-insensitive `Host:`, take leftmost label, `route_and_splice(inbound, peer, buf, sub, RouteMode::Http, state)`. Unknown ⇒ 404 + close. `run_https(state)` binds `https_port`; `serve_https` loops until the full ClientHello record is buffered (cap 8192), `parse_sni`, leftmost label, `route_and_splice(..., RouteMode::Https, ...)`; unknown/no-SNI ⇒ close. `route_and_splice` resolves `state.routes`, runs `ddos.analyze_connection`, opens stream via `RouteHandle.open_tx`, writes `encode_preamble(route_id, Some(peer), &replay)`, then `copy_bidirectional`, bumps `stats`.

---

## What is explicitly deferred (matches Readme Won't/Could + thesis)
- Cross-core-node connection forwarding (Redis `node:<id>` addr is written but not dialed).
- TLS termination / Host rewrite (passthrough only).
- WireGuard/boringtun real crypto, eBPF kernel drops, UDP hole punching (kept simulated).
- HTTP/2 prior-knowledge, QUIC/UDP :443.
