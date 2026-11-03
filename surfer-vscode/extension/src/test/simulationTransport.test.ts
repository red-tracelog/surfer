import * as assert from 'node:assert/strict'
import { describe, it } from 'node:test'
import { encodeLengthDelimitedFrame, MAX_SIMULATION_FRAME_SIZE, SimulationFrameDecoder } from '../simulationTransport'

describe('Simulation transport framing', () => {
  it('matches the documented two-byte wire prefix', () => {
    const chunk = Buffer.from([0x0a, 2, 0x22, 0, 0x10, 1, 0x18, 1])
    assert.deepEqual(encodeLengthDelimitedFrame(chunk), Buffer.from([0, 8, ...chunk]))
  })

  it('handles split headers, split payloads, and consecutive frames', () => {
    const frames = [Buffer.from([1]), Buffer.alloc(MAX_SIMULATION_FRAME_SIZE, 7), Buffer.from([3, 4])]
    const wire = Buffer.concat(frames.map(encodeLengthDelimitedFrame))
    for (const size of [1, 3, 1024, wire.length]) {
      const decoder = new SimulationFrameDecoder()
      const received: Buffer[] = []
      for (let offset = 0; offset < wire.length; offset += size) {
        decoder.push(wire.subarray(offset, offset + size), (frame) => received.push(frame))
      }
      decoder.finish()
      assert.deepEqual(received, frames)
    }
  })

  it('rejects empty incoming frames and oversized outgoing frames', () => {
    // A uint16 header cannot represent a size above the protocol maximum.
    const header = Buffer.alloc(2)
    assert.throws(() => new SimulationFrameDecoder().push(header, () => assert.fail('unexpected frame')))
    assert.throws(() => encodeLengthDelimitedFrame(Buffer.alloc(0)))
    assert.throws(() => encodeLengthDelimitedFrame(Buffer.alloc(MAX_SIMULATION_FRAME_SIZE + 1)))
  })

  it('reports EOF in either a header or payload', () => {
    const wire = encodeLengthDelimitedFrame(Buffer.from([1, 2, 3]))
    for (let length = 1; length < wire.length; length++) {
      const decoder = new SimulationFrameDecoder()
      decoder.push(wire.subarray(0, length), () => assert.fail('unexpected frame'))
      assert.throws(() => decoder.finish())
    }
    new SimulationFrameDecoder().finish()
  })
})
