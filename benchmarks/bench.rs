// bench.rs - the whole benchmark tool in one small file, no libraries.
// It has two modes, picked by the first argument:
//
//   bench serve <port>                          run the fake local service (the "origin")
//   bench load  <addr> <host> <path> <n> <secs> hit <addr> with <n> connections for <secs>
//
// "serve" is the thing being tunnelled; "load" is the client that measures speed.
// Compile once with:  rustc -O bench.rs -o bench
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map(String::as_str);
    match mode {
        Some("serve") => {
            let port = args[2].parse().unwrap();
            serve(port);
        }
        Some("load") => {
            let addr = &args[2];
            let host = &args[3];
            let path = &args[4];
            let conns = args[5].parse().unwrap();
            let secs = args[6].parse().unwrap();
            load(addr, host, path, conns, secs);
        }
        _ => {
            eprintln!("usage: bench serve <port> | bench load <addr> <host> <path> <conns> <secs>");
        }
    }
}

// ============================ the origin (a fake local service) ==============
// Answers every request with a body of N bytes, where N comes from the URL path
// (GET /64 -> 64 bytes, GET /10485760 -> 10 MiB). One thread per connection.
fn serve(port: u16) {
    let listener = TcpListener::bind(("127.0.0.1", port)).unwrap();
    for connection in listener.incoming() {
        let mut sock = connection.unwrap();
        thread::spawn(move || {
            sock.set_nodelay(true).ok(); // send replies immediately, don't batch
            let mut inbox = Vec::new(); // bytes received so far, not yet answered
            let mut buf = [0u8; 8192];
            loop {
                let n = match sock.read(&mut buf) {
                    Ok(0) | Err(_) => return, // connection closed
                    Ok(n) => n,
                };
                inbox.extend_from_slice(&buf[..n]);
                // a request head ends with a blank line ("\r\n\r\n"); answer each one
                while let Some(end) = find(&inbox, b"\r\n\r\n") {
                    let size = size_from_path(&inbox[..end]);
                    let head = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {size}\r\nConnection: keep-alive\r\n\r\n"
                    );
                    let body = vec![b'x'; size];
                    if sock.write_all(head.as_bytes()).is_err() {
                        return;
                    }
                    if sock.write_all(&body).is_err() {
                        return;
                    }
                    inbox.drain(..end + 4); // drop the request we just answered
                }
            }
        });
    }
}

// ============================ the load generator (the client) ================
// Opens <conns> keep-alive connections (one thread each), sends back-to-back
// requests for <secs> seconds, then prints one CSV line:
//   conns,requests,requests_per_sec,MiB_per_sec,p50_ms,p95_ms,p99_ms
fn load(addr: &str, host: &str, path: &str, conns: usize, secs: u64) {
    let stop = Arc::new(AtomicBool::new(false)); // set to true when the timer runs out
    let start = Instant::now();

    // start one worker thread per connection
    let mut workers = Vec::new();
    for _ in 0..conns {
        let addr = addr.to_string();
        let host = host.to_string();
        let path = path.to_string();
        let stop = stop.clone();
        workers.push(thread::spawn(move || one_connection(&addr, &host, &path, &stop)));
    }

    // let them run, then tell them to stop
    thread::sleep(Duration::from_secs(secs));
    stop.store(true, Ordering::Relaxed);

    // collect each worker's latencies and byte count
    let mut latencies = Vec::new(); // microseconds, one per request
    let mut bytes = 0u64;
    for w in workers {
        let (list, b) = w.join().unwrap();
        latencies.extend(list);
        bytes += b;
    }
    latencies.sort_unstable();
    let elapsed = start.elapsed().as_secs_f64();

    // Turn the raw counts into the reported figures, one value per line.
    let requests = latencies.len();
    let requests_per_sec = requests as f64 / elapsed;
    let mib_per_sec = bytes as f64 / 1_048_576.0 / elapsed;
    let p50 = percentile(&latencies, 0.50);
    let p95 = percentile(&latencies, 0.95);
    let p99 = percentile(&latencies, 0.99);
    println!("{conns},{requests},{requests_per_sec:.0},{mib_per_sec:.1},{p50:.3},{p95:.3},{p99:.3}");
}

// One connection's work: send a request, time the reply, repeat until stopped.
// Returns (latencies in microseconds, total bytes received).
fn one_connection(addr: &str, host: &str, path: &str, stop: &AtomicBool) -> (Vec<u64>, u64) {
    let mut latencies = Vec::new();
    let mut bytes = 0u64;
    let mut sock = match TcpStream::connect(addr) {
        Ok(s) => s,
        Err(_) => return (latencies, bytes),
    };
    sock.set_nodelay(true).ok();
    let request = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: keep-alive\r\n\r\n");
    let mut buf = [0u8; 65536];
    let mut leftover = Vec::new(); // bytes read past one reply, kept for the next
    while !stop.load(Ordering::Relaxed) {
        let sent_at = Instant::now();
        if sock.write_all(request.as_bytes()).is_err() {
            break;
        }
        match read_one_reply(&mut sock, &mut buf, &mut leftover) {
            Some(n) => {
                latencies.push(sent_at.elapsed().as_micros() as u64);
                bytes += n as u64;
            }
            None => break,
        }
    }
    (latencies, bytes)
}

// Read exactly one HTTP reply: the headers, then Content-Length body bytes.
fn read_one_reply(sock: &mut TcpStream, buf: &mut [u8], leftover: &mut Vec<u8>) -> Option<usize> {
    loop {
        if let Some(end) = find(leftover, b"\r\n\r\n") {
            let total = end + 4 + content_length(&leftover[..end]);
            while leftover.len() < total {
                let n = sock.read(buf).ok()?;
                if n == 0 {
                    return None;
                }
                leftover.extend_from_slice(&buf[..n]);
            }
            *leftover = leftover.split_off(total); // keep any bytes of the next reply
            return Some(total);
        }
        let n = sock.read(buf).ok()?;
        if n == 0 {
            return None;
        }
        leftover.extend_from_slice(&buf[..n]);
    }
}

// ============================ small helpers =================================
// find where `needle` first appears in `hay`
fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    for (index, window) in hay.windows(needle.len()).enumerate() {
        if window == needle {
            return Some(index);
        }
    }
    None
}
// pull the number out of a request line, e.g. "GET /64 HTTP/1.1" -> 64
fn size_from_path(head: &[u8]) -> usize {
    let text = String::from_utf8_lossy(head);
    let path = match text.split_whitespace().nth(1) {
        Some(second_word) => second_word,
        None => return 64,
    };
    let digits = path.trim_start_matches('/');
    digits.parse().unwrap_or(64)
}
// read the Content-Length header value out of a reply's head
fn content_length(head: &[u8]) -> usize {
    for line in String::from_utf8_lossy(head).lines() {
        if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            return v.trim().parse().unwrap_or(0);
        }
    }
    0
}
// The value at the p-th position of a sorted latency list (0.50 = median),
// converted from microseconds to milliseconds. An empty list reports 0.
fn percentile(sorted_micros: &[u64], p: f64) -> f64 {
    if sorted_micros.is_empty() {
        return 0.0;
    }
    let last_index = sorted_micros.len() - 1;
    let position = (last_index as f64 * p) as usize;
    let micros = sorted_micros[position];
    micros as f64 / 1000.0
}
