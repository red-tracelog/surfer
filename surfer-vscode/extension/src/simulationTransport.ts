// Keep in sync with surfer-simulator-protocol::MAX_FRAME_SIZE.
export const MAX_SIMULATION_FRAME_SIZE = 0xffff

function validateFrameLength(length: number): void {
  if (length === 0 || length > MAX_SIMULATION_FRAME_SIZE) {
    throw new Error(`Invalid simulation transport frame length: ${length}`)
  }
}

export function encodeLengthDelimitedFrame(frame: Buffer): Buffer {
  validateFrameLength(frame.length)
  const wire = Buffer.allocUnsafe(2 + frame.length)
  wire.writeUInt16BE(frame.length, 0)
  frame.copy(wire, 2)
  return wire
}

// Forward bounded protobuf chunks unchanged. Reassembly belongs to Rust/WASM.
// Allocate once per frame and retain at most one incomplete frame between reads.
export class SimulationFrameDecoder {
  private readonly header = Buffer.alloc(2)
  private headerUsed = 0
  private frame: Buffer | undefined
  private frameUsed = 0

  push(chunk: Buffer, emit: (frame: Buffer) => void): void {
    let offset = 0
    while (offset < chunk.length) {
      if (this.frame === undefined) {
        const count = Math.min(2 - this.headerUsed, chunk.length - offset)
        chunk.copy(this.header, this.headerUsed, offset, offset + count)
        this.headerUsed += count
        offset += count
        if (this.headerUsed < 2) {
          return
        }
        const length = this.header.readUInt16BE(0)
        validateFrameLength(length)
        this.frame = Buffer.allocUnsafe(length)
        this.frameUsed = 0
        this.headerUsed = 0
      }

      const count = Math.min(this.frame.length - this.frameUsed, chunk.length - offset)
      chunk.copy(this.frame, this.frameUsed, offset, offset + count)
      this.frameUsed += count
      offset += count
      if (this.frameUsed === this.frame.length) {
        const frame = this.frame
        this.frame = undefined
        this.frameUsed = 0
        emit(frame)
      }
    }
  }

  finish(): void {
    if (this.headerUsed !== 0 || this.frame !== undefined) {
      throw new Error('Incomplete simulation transport frame')
    }
  }
}
