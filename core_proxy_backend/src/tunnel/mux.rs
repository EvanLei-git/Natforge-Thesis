//! yamux client driver (pipelined).
//!
//! yamux 0.14 exposes a poll-based `Connection` with no separate control handle, so
//! one task owns the connection, keeps it serviced, and fulfils outbound-stream
//! requests. To avoid head-of-line blocking across routes (a burst on one route
//! must not stall opens for another), this driver pipelines: it greedily drains
//! the request channel into a queue and keeps calling `poll_new_outbound` until it
//! returns `Pending`, completing as many opens per wake-up as the connection allows.
//!
//! It is a single `poll_fn` state machine, so it holds exactly one mutable borrow
//! of the connection (the borrow checker rejects driving it from multiple
//! `select!` arms - which is the correct constraint).

use std::collections::VecDeque;
use std::task::Poll;

use futures::future::poll_fn;
use tokio::sync::{mpsc, oneshot};
use yamux::{Connection, ConnectionError, Stream};

use crate::state::OpenStream;

pub async fn run_client_driver<T>(mut conn: Connection<T>, mut open_rx: mpsc::Receiver<OpenStream>)
where
    T: futures::AsyncRead + futures::AsyncWrite + Unpin,
{
    // Opens accepted from the channel but not yet handed a stream.
    let mut queue: VecDeque<oneshot::Sender<Result<Stream, ConnectionError>>> = VecDeque::new();
    let mut channel_open = true;

    poll_fn(|cx| {
        loop {
            // 1. Service inbound frames (pings, window updates). The agent never
            //    opens streams toward us, so any inbound stream is dropped.
            match conn.poll_next_inbound(cx) {
                Poll::Ready(Some(Ok(_unexpected))) => continue,
                Poll::Ready(Some(Err(_))) | Poll::Ready(None) => {
                    for reply in queue.drain(..) {
                        let _ = reply.send(Err(ConnectionError::Closed));
                    }
                    return Poll::Ready(());
                }
                Poll::Pending => {}
            }

            // 2. Greedily drain pending open-requests into the queue.
            while channel_open {
                match open_rx.poll_recv(cx) {
                    Poll::Ready(Some(req)) => queue.push_back(req.reply),
                    Poll::Ready(None) => {
                        channel_open = false; // tunnel dropped; finish what we have
                    }
                    Poll::Pending => break,
                }
            }

            // 3. Satisfy as many queued opens as the connection will give us now.
            let mut made_progress = false;
            while !queue.is_empty() {
                match conn.poll_new_outbound(cx) {
                    Poll::Ready(res) => {
                        let reply = queue.pop_front().expect("queue non-empty");
                        let _ = reply.send(res);
                        made_progress = true;
                    }
                    Poll::Pending => break,
                }
            }

            // 4. Decide whether to yield.
            if !channel_open && queue.is_empty() {
                return Poll::Ready(());
            }
            if !made_progress {
                // Nothing more to do until inbound, a new request, or outbound-open
                // capacity wakes us (all three registered their wakers above).
                return Poll::Pending;
            }
            // Made progress; loop to re-drain/re-poll.
        }
    })
    .await;
}
