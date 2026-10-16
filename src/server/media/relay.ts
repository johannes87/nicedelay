import net from 'node:net';

// When OBS stops streaming on purpose, it sends the RTMP commands "FCUnpublish" and
// "deleteStream" before closing the connection; a crash or network drop just closes it.
// Commands are AMF0 strings on the wire: 0x02, a 16-bit length, then the name.
const MARKERS = ['FCUnpublish', 'deleteStream'].map((name) =>
  Buffer.concat([Buffer.from([0x02, 0x00, name.length]), Buffer.from(name, 'ascii')]),
);
const KEEP = Math.max(...MARKERS.map((m) => m.length)) - 1;

/** Watches the bytes OBS sends for its "stop streaming" commands. */
export class StopDetector {
  seen = false;
  private tail = Buffer.alloc(0);

  /** Feed the next chunk of the stream; returns true once a stop command was seen. */
  push(chunk: Buffer): boolean {
    if (this.seen) return true;
    // Keep the end of the previous chunk, so a command split across chunks is still found.
    const data = this.tail.length ? Buffer.concat([this.tail, chunk]) : chunk;
    this.seen = MARKERS.some((m) => data.includes(m));
    this.tail = Buffer.from(data.subarray(Math.max(0, data.length - KEEP)));
    return this.seen;
  }
}

/** A currently free TCP port on 127.0.0.1. */
export function freePort(): Promise<number> {
  const { promise, resolve, reject } = Promise.withResolvers<number>();
  const server = net.createServer();
  server.once('error', reject);
  server.listen(0, '127.0.0.1', () => {
    const { port } = server.address() as net.AddressInfo;
    server.close(() => resolve(port));
  });
  return promise;
}
