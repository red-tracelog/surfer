# Surfer Simulator Protocol

This protocol is used for real-time communication between Surfer and external simulators. It was created for Surfer to communicate with NVC using a VHPI plugin. Much of the implementation is based on how Surfer and wellen handle VHDL data.

This document explains the protocol structure so implementations can be made for other simulators. It is not fully comprehensive, and focuses on what is not already covered by schema/comments. Refer to the VHPI plugin for a reference implementation.

Surfer connects to a simulator/plugin over TCP (default port 6967). Protocol version **1** uses Protocol Buffers with bounded transport chunks in both directions. The schema is [proto/simulator_protocol.proto](proto/simulator_protocol.proto). Generate bindings for your language from this schema; the Rust implementation is not required.

## TCP framing and chunking

Each TCP frame has this layout:

```text
2-byte unsigned big-endian length N | N bytes of protobuf TransportChunk
```

The prefix counts only the encoded `TransportChunk`, not itself. This is a fixed two-byte prefix, **not** a protobuf varint length prefix. Set the maximum frame length to **65,535 bytes**. For example, Rust's `tokio_util::codec::LengthDelimitedCodec` defaults to a four-byte prefix and an 8 MiB maximum; configure `.length_field_length(2).max_frame_length(MAX_FRAME_SIZE)` explicitly.

A logical message is an encoded `SimulatorToSurfer` or `SurferToSimulator`, according to direction. To send one:

1. Encode the directional message as protobuf bytes.
2. Split these bytes into nonempty pieces. Rust uses **65,503 bytes** per piece, leaving 32 bytes for envelope overhead. Smaller pieces are valid too. Splits may occur anywhere, including inside a protobuf field or a UTF-8 sequence.
3. Wrap each piece in `TransportChunk { data, last, protocol_version: 1 }`. Set `last` to true only for the final piece. Even a small message uses one chunk with `last = true`.
4. Encode each envelope, verify its size is at most 65,535 bytes, and send its two-byte length followed by its bytes.
5. Send all chunks of one logical message before starting another in the same direction. Use one writer task or hold a write lock across the entire logical message. The two TCP directions are independent.

To receive, read exactly two bytes, validate `1 <= N <= 65535`, then read exactly N bytes. TCP reads can split headers and payloads or combine several frames. Decode each envelope, require `protocol_version == 1` and nonempty `data`, and append its `data` to the current logical message. Only when `last` is true, decode the accumulated bytes as the appropriate directional message and clear the accumulator. Do not try to decode an individual piece as a directional message.

Close the connection on malformed envelopes, unsupported versions, or invalid lengths. EOF inside a frame or before `last = true` is truncation: discard the partial message. Never carry partial bytes across connections. A canceled partial write also requires closing the connection. Unknown protobuf fields follow the usual protobuf rules.

For a concrete wire example, `SurferToSimulator { request_hierarchy: {} }` encodes to `22 00`. Its single chunk is `0a 02 22 00 10 01 18 01`, and the complete TCP frame is:

```text
00 08 0a 02 22 00 10 01 18 01
```

There is no fixed limit on the total number of chunks or aggregate logical-message size in this protocol. Chunking bounds individual frames, not total memory: the current receivers assemble a complete logical message before protobuf decoding, so available memory and the protobuf implementation's own limits still apply. Plugins batch ordinary value updates around 1 MiB to avoid unnecessarily large logical messages; an individual large timestep stays intact and uses multiple transport chunks. `last` ends a logical message; it is independent of `ValueChanges.status`. Intermediate value batches use `Running`, and the final batch carries `Paused` or `Done`, including an empty batch when needed to deliver terminal status.

The VS Code extension forwards one encoded `TransportChunk` per base64 bridge message, without the TCP length prefix. Rust/WASM performs logical-message reassembly; the proxy does not combine chunks into a large bridge message. No buffer-size startup or CLI arguments are needed.

### Python example

Generate `simulator_protocol_pb2.py` using `protoc --python_out=. --proto_path=proto proto/simulator_protocol.proto` (with the protobuf Python runtime installed). The following blocking-socket helpers implement the same wire format. Serialize concurrent writes around the entire `send_message` call.

```python
import struct
from simulator_protocol_pb2 import TransportChunk, SimulatorToSurfer, SurferToSimulator

MAX_FRAME = 65535
CHUNK_DATA = MAX_FRAME - 32

def read_exact(sock, size):
    data = bytearray()
    while len(data) < size:
        part = sock.recv(size - len(data))
        if not part:
            raise EOFError("connection closed; discard any partial message")
        data.extend(part)
    return data

def send_message(sock, message):
    payload = message.SerializeToString()
    if not payload:
        raise ValueError("missing directional message kind")
    for offset in range(0, len(payload), CHUNK_DATA):
        piece = payload[offset:offset + CHUNK_DATA]
        envelope = TransportChunk(
            data=piece,
            last=offset + len(piece) == len(payload),
            protocol_version=1,
        ).SerializeToString()
        assert len(envelope) <= MAX_FRAME
        sock.sendall(struct.pack(">H", len(envelope)) + envelope)

def receive_message(sock, message_type):
    payload = bytearray()
    while True:
        size, = struct.unpack(">H", read_exact(sock, 2))
        if not 1 <= size <= MAX_FRAME:
            raise ValueError("invalid frame length; close connection")
        chunk = TransportChunk.FromString(bytes(read_exact(sock, size)))
        if chunk.protocol_version != 1 or not chunk.data:
            raise ValueError("invalid transport chunk; close connection")
        payload.extend(chunk.data)
        if chunk.last:
            return message_type.FromString(bytes(payload))

# A plugin receives commands using:
# command = receive_message(sock, SurferToSimulator)
# It sends responses using send_message(sock, SimulatorToSurfer(...)).
```

The same algorithm applies to C++, Go, Java, and other protobuf runtimes: encode the directional message, split its bytes, then encode and frame each `TransportChunk`. Do not use a runtime's varint-delimited stream API for the TCP prefix.

## Shared Rust plugin support

`surfer-simulator-protocol` provides the simulator-independent code used by both VPI and VHPI:

- `plugin::StartupOptions::parse` handles `+address`/`+port`, `+debug`, and `+no-info` with shared defaults and address normalization.
- `plugin::SimulatorCommand` queues requests and their response channels for execution on the simulator thread.
- `plugin::CallbackStore<Handle>` tracks callback handles by signal ID. Each backend supplies its own `SafeHandle`; removal transfers the handle back to the backend for simulator-specific unregistration. Replaced handles are dropped after releasing the store lock.
- With the optional `plugin-tcp` feature, `plugin::tcp::serve` accepts one Surfer session, sends initial simulator metadata, dispatches requests, and writes responses and asynchronous updates through one framed writer. It acknowledges shutdown before queuing `Finish`, and queues `Finish` on bind or accept failure. Simulator APIs remain in the plugins; the shared TCP task only uses channels and encoded messages. The TCP module is native-only.
- `plugin::uint_to_bit_string` and `plugin::int_to_bit_string` format fixed-width binary values for both backends.
- `plugin::ClientConnection` queues complete encoded messages from simulator callbacks to the connection task. `send_value_changes` batches updates and preserves terminal statuses.
- `plugin::encode_value_change_batches` exposes the batching logic with an output callback for plugins with their own queue.
- `transport::encode_chunks` yields bounded encoded envelopes for a logical message.
- `transport::write_message` writes all envelopes to a `Sink<Bytes>`, such as a `FramedWrite` with `LengthDelimitedCodec`.
- `transport::MessageAssembler` maintains receive state. Call `push` for each decoded TCP frame; `Some(bytes)` means a complete directional message. Call `finish` at EOF to detect an unfinished message, and drop the assembler on disconnect or error.

Configure both reader and writer codecs with `MAX_FRAME_SIZE`. Keep a separate assembler per connection and direction. Queue logical messages, then chunk them in the single writer task, so responses and asynchronous updates cannot interleave their chunks. The provided callback queue is unbounded; plugins that need a strict total memory bound should add producer backpressure separately.

## Timeline

Startup and normal flow are:

1. Simulator send `SimulatorInfo` upon connection, including `protocol_version`, `name`, and `version`.
2. Surfer sends `RequestHierarchy`.
3. Simulator replies with `Hierarchy`.
4. Surfer sends `TrackVariableChanges` and `UntrackVariableChanges` as the user selects/deselects variables.
5. Surfer sends `RunSimulation` (optionally with a relative duration) and `PauseSimulation`.
6. Simulator sends `ValueChanges` and optionally `CurrentSimulationTime` updates while running.

## Hierarchy

The hierarchy is a vector of `HierarchyElement` values. These can be `PushScope`, `PopScope`, and `AddVar`. Every `PushScope` must be paired with a matching `PopScope`.

`PushScope` and `PopScope` should be used for arrays and records/structs. For arrays, it is recommended that the name is the array index. Use `AddVar` for leaf variables. For names, use simulator-native naming consistently so the frontend can map variables correctly.

`Hierarchy` includes a decimal timestamp exponent so Surfer knows how simulator timestamps map to time units used in `RunSimulation` and `TimeStep` timestamps.

Durations and timestamps are nonnegative integer simulator ticks. The Rust
`SimulationDuration` and `SimulationTimestamp` types distinguish relative and
absolute time when validating backend conversions; the wire fields remain `uint64`.
VPI supports unsigned 64-bit ticks; VHPI supports nonnegative signed 64-bit ticks.
Plugins reject runs whose duration or absolute deadline exceeds the backend's
range before changing callbacks. `RunSimulation.time` is a protobuf `oneof` with
`relative` (duration, field 1) and `absolute` (timestamp, field 2) alternatives.
Rust represents these as `RunTime::Relative(SimulationDuration)` and
`RunTime::Absolute(SimulationTimestamp)`. Surfer currently sends relative durations;
both plugins accept either form. An absolute deadline in the past is rejected.
A zero duration or an absolute deadline equal to now pauses at the end of the
current timestep; an absent limit runs until completion. Negative VHPI timestamps are
rejected rather than reported as zero.

The `plugin` module provides shared `SimulatorData` metadata message builders and
`ValueChangeBuffer` recording. Backends convert native values and timestamps, then
record changes in callback order. The buffer groups consecutive equal timestamps,
preserves repeated changes within a timestep, and removes pending values when
signals are untracked. Backends retain ownership of hierarchy traversal, handles,
and callback registration. Drain the buffer before encoding or sending to release
the recording lock during output.

## Variable id:s

The simulator (plugin) is responsible for creating a unique id for each variable in the hierarchy.

## Variable Tracking

`TrackVariableChanges` can be sent for the same variable multiple times. Only the first call should have an effect. After `UntrackVariableChanges`, variable changes for that variable should no longer be sent.

## Signal Changes

Signal value changes should use an encoding consistent with each variable's declared `SignalEncoding` in `Hierarchy`/`AddVar`.
