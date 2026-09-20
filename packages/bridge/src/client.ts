// TCP transport: frames are `u32 length | wire bytes`, matching
// crates/craie/src/bridge.rs.

import { Socket } from "node:net"
import type { Transport } from "./host.js"

export class TcpTransport implements Transport {
  private socket: Socket

  /** Use `connect` — the constructor exists for tests and custom sockets. */
  constructor(socket: Socket) {
    this.socket = socket
  }

  send(frame: Uint8Array) {
    const head = Buffer.alloc(4)
    head.writeUInt32LE(frame.byteLength)
    this.socket.write(Buffer.concat([head, Buffer.from(frame)]))
  }

  close(_reason?: string) {
    this.socket.destroy()
  }
}

export function connect(port = 9470, host = "127.0.0.1"): Promise<TcpTransport> {
  return new Promise((resolve, reject) => {
    const socket = new Socket()
    socket.once("error", reject)
    socket.connect(port, host, () => {
      socket.setNoDelay(true)
      resolve(new TcpTransport(socket))
    })
  })
}
