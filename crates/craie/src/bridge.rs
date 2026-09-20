//! Process boundary between the JS driver and the native app.
//!
//! Transport is a localhost TCP socket carrying length-prefixed wire
//! transactions (`u32` byte length + `wire::decode`-compatible payload).
//! A reader thread per connection blocks on the socket, pushes frames into
//! a channel, and pokes the event loop through `Wake` — the main thread
//! drains the inbox in `App::woke`, applies transactions, and repaints
//! once. The socket thread never touches host state.
//!
//! This is deliberately dumb. The gpui-react bridge moved bytes over a
//! napi shared buffer; TCP preserves the same properties that matter here
//! (opaque bytes, wake-based delivery, no JS on the main thread) without
//! a native-addon toolchain.

use std::io::{self, Read};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::thread;

use crate::platform::Wake;

/// Receives wire transaction buffers from connected drivers.
pub struct Inbox {
    rx: Receiver<Vec<u8>>,
    /// Local port the listener is bound to.
    pub port: u16,
}

impl Inbox {
    /// Drains all pending frames.
    pub fn drain(&self) -> impl Iterator<Item = Vec<u8>> + '_ {
        self.rx.try_iter()
    }
}

/// Binds `addr` (e.g. `127.0.0.1:0`) and spawns the accept thread.
/// Every accepted connection gets a reader thread that frames
/// `u32 len | payload` messages into the shared inbox and wakes the loop.
pub fn listen(addr: &str, wake: Wake) -> io::Result<Inbox> {
    let listener = TcpListener::bind(addr)?;
    let port = listener.local_addr()?.port();
    let (tx, rx) = sync_channel::<Vec<u8>>(256);
    thread::Builder::new()
        .name("craie-accept".into())
        .spawn(move || {
            for stream in listener.incoming() {
                match stream {
                    Ok(s) => spawn_reader(s, tx.clone(), wake.clone()),
                    Err(_) => break,
                }
            }
        })?;
    Ok(Inbox { rx, port })
}

fn spawn_reader(stream: TcpStream, tx: SyncSender<Vec<u8>>, wake: Wake) {
    let _ = stream.set_nodelay(true);
    thread::Builder::new()
        .name("craie-bridge-rx".into())
        .spawn(move || {
            let mut stream = stream;
            let mut len_buf = [0u8; 4];
            loop {
                if stream.read_exact(&mut len_buf).is_err() {
                    return;
                }
                let len = u32::from_le_bytes(len_buf) as usize;
                if len > 64 * 1024 * 1024 {
                    return; // absurd frame; drop the connection
                }
                let mut buf = vec![0u8; len];
                if stream.read_exact(&mut buf).is_err() {
                    return;
                }
                if tx.send(buf).is_err() {
                    return; // app gone
                }
                wake.wake();
            }
        })
        .ok();
}
