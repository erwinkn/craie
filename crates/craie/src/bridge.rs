//! Process boundary between the JS driver and the native app.
//!
//! Transport is a localhost TCP socket carrying length-prefixed wire
//! transactions (`u32` byte length + `wire::decode`-compatible payload).
//! A reader thread per connection blocks on the socket, pushes frames into
//! a channel, and pokes the event loop through `Wake` — the main thread
//! drains the inbox in `App::woke`, applies transactions, and repaints
//! once. The socket thread never touches host state.
//!
//! Acknowledgements flow back on the same socket as bare `u64` seqs
//! (`AckSink::ack`). The JS side holds removed ids until the transaction
//! that removed them is acknowledged, then recycles them — the gpui-react
//! contract, which keeps id reuse strictly ordered behind native apply.
//!
//! This is deliberately dumb. The gpui-react bridge moved bytes over a
//! napi shared buffer; TCP preserves the same properties that matter here
//! (opaque bytes, wake-based delivery, no JS on the main thread) without
//! a native-addon toolchain.

use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread;

use crate::platform::Wake;

/// Receives wire transaction buffers from connected drivers.
pub struct Inbox {
    rx: Receiver<Vec<u8>>,
    /// Local port the listener is bound to.
    pub port: u16,
    /// Acks applied seqs back to the most recent connection.
    pub acks: AckSink,
}

impl Inbox {
    /// Drains all pending frames.
    pub fn drain(&self) -> impl Iterator<Item = Vec<u8>> + '_ {
        self.rx.try_iter()
    }
}

/// Writes `u64` seq acknowledgements on the live connection, if any.
/// Replaced on each new connection; a lost connection just drops acks.
#[derive(Clone, Default)]
pub struct AckSink {
    writer: Arc<Mutex<Option<TcpStream>>>,
}

impl AckSink {
    pub fn ack(&self, seq: u64) {
        if let Some(s) = self.writer.lock().unwrap().as_mut() {
            let _ = s.write_all(&seq.to_le_bytes());
        }
    }
}

/// Binds `addr` (e.g. `127.0.0.1:0`) and spawns the accept thread.
/// Every accepted connection gets a reader thread that frames
/// `u32 len | payload` messages into the shared inbox and wakes the loop.
pub fn listen(addr: &str, wake: Wake) -> io::Result<Inbox> {
    let listener = TcpListener::bind(addr)?;
    let port = listener.local_addr()?.port();
    let (tx, rx) = sync_channel::<Vec<u8>>(256);
    let acks = AckSink::default();
    let acks2 = acks.clone();
    thread::Builder::new()
        .name("craie-accept".into())
        .spawn(move || {
            for stream in listener.incoming() {
                match stream {
                    Ok(s) => spawn_reader(s, tx.clone(), wake.clone(), acks2.clone()),
                    Err(_) => break,
                }
            }
        })?;
    Ok(Inbox { rx, port, acks })
}

fn spawn_reader(stream: TcpStream, tx: SyncSender<Vec<u8>>, wake: Wake, acks: AckSink) {
    let _ = stream.set_nodelay(true);
    if let Ok(w) = stream.try_clone() {
        *acks.writer.lock().unwrap() = Some(w);
    }
    thread::Builder::new()
        .name("craie-bridge-rx".into())
        .spawn(move || {
            let mut stream = stream;
            let mut len_buf = [0u8; 4];
            loop {
                if stream.read_exact(&mut len_buf).is_err() {
                    break;
                }
                let len = u32::from_le_bytes(len_buf) as usize;
                if len > 64 * 1024 * 1024 {
                    break; // absurd frame; drop the connection
                }
                let mut buf = vec![0u8; len];
                if stream.read_exact(&mut buf).is_err() {
                    break;
                }
                if tx.send(buf).is_err() {
                    break; // app gone
                }
                wake.wake();
            }
            // Connection closed: stop acking it.
            let mut guard = acks.writer.lock().unwrap();
            if let Some(w) = guard.as_ref()
                && w.peer_addr().ok() == stream.peer_addr().ok()
            {
                *guard = None;
            }
        })
        .ok();
}
