export class TextEncoder {
  encode(s: string): Uint8Array {
    return <Uint8Array>__blueboat_host_invoke("encode", s);
  }
}

export class TextDecoder {
  decode(s: Uint8Array): string {
    return <string>__blueboat_host_invoke("decode", s);
  }
}
