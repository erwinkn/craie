//! In-process bridge between the JS/React thread and the native UI thread.
//!
//! React encodes one transaction per commit and hands it to `submit` from
//! its own thread. `submit` pushes the bytes into the shared queue and
//! wakes the event loop; the UI thread drains in `App::woke`, applies
//! atomically, and pushes the applied seq into `acks`. Acks gate id
//! recycling on the JS side — reuse can never race native apply.
//!
//! Modeled on the gpui-react session: a bounded `VecDeque` behind a mutex,
//! a condvar for the JS-side `receive`, and a wake for the UI loop — not
//! a shared-memory ring. One copied commit per React transaction; measure
//! before reaching for anything fancier.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};

use crate::platform::Wake;

const MAX_TRANSACTIONS: usize = 256;
const MAX_BYTES: usize = 4 * 1024 * 1024;

/// One live connection between a React runtime and a native UI.
///
/// The host (UI thread) holds an `Arc<Session>` and drains `commits`;
/// the client (JS thread) holds another and calls `submit`. `Wake` is
/// installed once the platform loop exists; submits before then still
/// queue and are drained by the app's initial sync.
#[derive(Default)]
pub struct Session {
    inner: Mutex<Inner>,
    /// Signaled when `acks` gains entries or the session closes.
    changed: Condvar,
}

#[derive(Default)]
struct Inner {
    /// Encoded transactions awaiting apply, JS -> UI.
    commits: VecDeque<Vec<u8>>,
    commit_bytes: usize,
    /// Applied seqs awaiting JS delivery, UI -> JS.
    acks: VecDeque<u64>,
    /// Platform-loop poke, installed when the window starts.
    wake: Option<Wake>,
    /// Reason the session ended; submission and receive both fail after.
    closed: Option<String>,
}

impl Session {
    pub fn new() -> Arc<Session> {
        Arc::new(Session::default())
    }

    /// Queues one encoded transaction and wakes the UI loop.
    /// The copy happened on the caller's side; this is the only hop.
    pub fn submit(&self, txn: Vec<u8>) -> Result<(), String> {
        let wake = {
            let mut inner = self.inner.lock().unwrap();
            if let Some(reason) = &inner.closed {
                return Err(reason.clone());
            }
            if inner.commits.len() >= MAX_TRANSACTIONS
                || inner.commit_bytes + txn.len() > MAX_BYTES
            {
                return Err("commit queue is full".to_string());
            }
            inner.commit_bytes += txn.len();
            inner.commits.push_back(txn);
            inner.wake.clone()
        };
        if let Some(wake) = wake {
            wake.wake();
        }
        Ok(())
    }

    /// UI thread: takes every queued transaction. One lock per wake.
    pub fn take_commits(&self) -> VecDeque<Vec<u8>> {
        let mut inner = self.inner.lock().unwrap();
        inner.commit_bytes = 0;
        std::mem::take(&mut inner.commits)
    }

    /// Installed by the platform glue once the event loop exists.
    pub fn install_wake(&self, wake: Wake) {
        self.inner.lock().unwrap().wake = Some(wake);
    }

    /// UI thread: records `seq` as applied and wakes any waiting
    /// `receive` call on the JS side.
    pub fn ack(&self, seq: u64) {
        {
            self.inner.lock().unwrap().acks.push_back(seq);
        }
        self.changed.notify_all();
    }

    /// JS thread: drains pending acks without blocking.
    pub fn take_acks(&self) -> Vec<u64> {
        self.inner.lock().unwrap().acks.drain(..).collect()
    }

    /// JS thread: blocks until acks are pending or the session closes,
    /// then returns them (empty on close). Never call on the UI thread.
    pub fn recv_acks(&self) -> Vec<u64> {
        let mut inner = self.inner.lock().unwrap();
        loop {
            if !inner.acks.is_empty() {
                return inner.acks.drain(..).collect();
            }
            if inner.closed.is_some() {
                return Vec::new();
            }
            inner = self.changed.wait(inner).unwrap();
        }
    }

    /// Ends the session; pending commits are dropped, `submit` and
    /// `recv_acks` both fail/return empty, and the UI loop wakes so it
    /// can observe the close.
    pub fn close(&self, reason: impl Into<String>) {
        let wake = {
            let mut inner = self.inner.lock().unwrap();
            if inner.closed.is_none() {
                inner.closed = Some(reason.into());
            }
            inner.commits.clear();
            inner.wake.clone()
        };
        if let Some(wake) = wake {
            wake.wake();
        }
        self.changed.notify_all();
    }

    pub fn closed_reason(&self) -> Option<String> {
        self.inner.lock().unwrap().closed.clone()
    }

    pub fn is_closed(&self) -> bool {
        self.inner.lock().unwrap().closed.is_some()
    }
}

/// Registry of live sessions by id, so a JS worker can attach to the
/// session its host created (`NativeHost` hands the id through
/// `workerData`; `NativeClient` upgrades it here).
#[derive(Default)]
pub struct Sessions {
    map: Mutex<HashMap<u32, Weak<Session>>>,
    next: AtomicU32,
}

impl Sessions {
    pub fn new() -> Sessions {
        Sessions::default()
    }

    pub fn insert(&self, session: &Arc<Session>) -> u32 {
        let id = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        self.map
            .lock()
            .unwrap()
            .insert(id, Arc::downgrade(session));
        id
    }

    pub fn get(&self, id: u32) -> Option<Arc<Session>> {
        self.map.lock().unwrap().get(&id).and_then(Weak::upgrade)
    }

    pub fn remove(&self, id: u32) {
        self.map.lock().unwrap().remove(&id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn submit_queues_and_acks_round_trip() {
        let s = Session::new();
        s.submit(b"txn1".to_vec()).unwrap();
        s.submit(b"txn2".to_vec()).unwrap();
        let commits = s.take_commits();
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0], b"txn1");
        assert!(s.take_commits().is_empty());

        s.ack(41);
        s.ack(42);
        assert_eq!(s.recv_acks(), vec![41, 42]);
    }

    #[test]
    fn close_rejects_submit_and_unblocks_recv() {
        let s = Session::new();
        let s2 = s.clone();
        let t = std::thread::spawn(move || s2.recv_acks());
        s.submit(b"x".to_vec()).unwrap();
        s.close("done");
        assert!(t.join().unwrap().is_empty());
        assert!(s.submit(b"y".to_vec()).is_err());
    }
}
