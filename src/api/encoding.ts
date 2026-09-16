// Base64 at the terminal edge. Ported from demo-studio's `platform/consoleTransport.ts`.

/** Text → base64. Symmetric with `decodeB64`. */
export function encodeB64(text: string): string {
  const bytes = new TextEncoder().encode(text);
  let binary = "";
  for (let i = 0; i < bytes.length; i++) binary += String.fromCharCode(bytes[i]);
  return btoa(binary);
}

/** The bytes inside a base64 chunk. Returns a `Uint8Array` and NEVER a string: a chunk boundary can
 *  fall inside a multi-byte UTF-8 sequence, and decoding each chunk independently would turn the
 *  split codepoint into two replacement characters. xterm's writer takes bytes and does its own
 *  incremental decoding across chunks, which is the only layer positioned to do it correctly. */
export function decodeB64(b64: string): Uint8Array {
  const binary = atob(b64);
  const out = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) out[i] = binary.charCodeAt(i);
  return out;
}
