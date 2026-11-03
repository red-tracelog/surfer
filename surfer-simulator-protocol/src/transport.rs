//! Bounded protobuf chunks shared by TCP connections and the VS Code bridge.
use futures::{Sink, SinkExt};
use prost::{Message, bytes::Bytes};

use crate::{MAX_FRAME_SIZE, PROTOCOL_VERSION, ProtocolError, proto::TransportChunk};

// Leave room for tags, the bytes length, last, and protocol_version.
pub const CHUNK_DATA_SIZE: usize = MAX_FRAME_SIZE - 32;

/// Encoded chunk envelopes, without the two-byte big-endian TCP length prefix.
/// A message must be nonempty. Chunks of different messages must not interleave.
pub fn encode_chunks(message: &[u8]) -> impl Iterator<Item = Vec<u8>> + '_ {
    assert!(
        !message.is_empty(),
        "a directional protobuf message must be nonempty"
    );
    message
        .chunks(CHUNK_DATA_SIZE)
        .enumerate()
        .map(|(index, data)| {
            TransportChunk {
                data: data.to_vec(),
                last: index == (message.len() - 1) / CHUNK_DATA_SIZE,
                protocol_version: PROTOCOL_VERSION,
            }
            .encode_to_vec()
        })
}

/// Write every chunk before another message is written to this sink.
/// Cancellation partway through a message requires closing the connection.
pub async fn write_message<S>(sink: &mut S, message: &[u8]) -> Result<(), S::Error>
where
    S: Sink<Bytes> + Unpin,
{
    for chunk in encode_chunks(message) {
        sink.send(chunk.into()).await?;
    }
    Ok(())
}

/// One instance per connection and direction. Drop it on disconnect or error.
/// Chunks are bounded; the assembled logical message still requires memory.
#[derive(Default)]
pub struct MessageAssembler {
    pending: Vec<u8>,
}

impl MessageAssembler {
    /// Returns a complete directional protobuf message only on the last chunk.
    pub fn push(&mut self, frame: &[u8]) -> Result<Option<Vec<u8>>, ProtocolError> {
        let result = self.push_inner(frame);
        if result.is_err() {
            self.pending.clear();
        }
        result
    }

    fn push_inner(&mut self, frame: &[u8]) -> Result<Option<Vec<u8>>, ProtocolError> {
        if frame.len() > MAX_FRAME_SIZE {
            return Err(ProtocolError::InvalidValue("TransportChunk size"));
        }
        let chunk = TransportChunk::decode(frame)?;
        if chunk.protocol_version != PROTOCOL_VERSION {
            return Err(ProtocolError::InvalidValue(
                "TransportChunk.protocol_version",
            ));
        }
        if chunk.data.is_empty() {
            return Err(ProtocolError::InvalidValue("TransportChunk.data"));
        }
        self.pending.extend_from_slice(&chunk.data);
        Ok(chunk.last.then(|| std::mem::take(&mut self.pending)))
    }

    /// Call at EOF to distinguish a clean close from a truncated message.
    pub fn finish(&self) -> Result<(), ProtocolError> {
        if self.pending.is_empty() {
            Ok(())
        } else {
            Err(ProtocolError::InvalidValue("incomplete chunked message"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        HierarchyElement, ScopeType, SignalChange, SignalValue, SimulationStatus,
        SimulatorToSurferMessage, SurferToSimulatorMessage, TimeStep,
        decode_simulator_to_surfer_message, decode_surfer_to_simulator_message,
        encode_simulator_to_surfer_message, encode_surfer_to_simulator_message,
    };
    use futures::StreamExt;
    use tokio_util::codec::{FramedRead, FramedWrite, LengthDelimitedCodec};

    fn roundtrip(encoded: &[u8]) -> Vec<u8> {
        let mut assembler = MessageAssembler::default();
        let chunks: Vec<_> = encode_chunks(encoded).collect();
        let mut message = None;
        for (index, chunk) in chunks.iter().enumerate() {
            assert!(chunk.len() <= MAX_FRAME_SIZE);
            let result = assembler.push(chunk).unwrap();
            assert_eq!(result.is_some(), index == chunks.len() - 1);
            if result.is_some() {
                message = result;
            }
        }
        assembler.finish().unwrap();
        message.unwrap()
    }

    #[test]
    fn chunk_boundaries_and_wire_example() {
        let request =
            encode_surfer_to_simulator_message(&SurferToSimulatorMessage::RequestHierarchy)
                .unwrap();
        assert_eq!(
            encode_chunks(&request).collect::<Vec<_>>(),
            vec![vec![0x0a, 2, 0x22, 0, 0x10, 1, 0x18, 1]]
        );
        for length in [
            1,
            CHUNK_DATA_SIZE - 1,
            CHUNK_DATA_SIZE,
            CHUNK_DATA_SIZE + 1,
            CHUNK_DATA_SIZE * 2,
        ] {
            let encoded = vec![0x42; length];
            assert_eq!(roundtrip(&encoded), encoded);
        }
    }

    #[test]
    fn hierarchy_single_timestep_and_tracking_can_exceed_default_codec_limit() {
        // Tokio's LengthDelimitedCodec defaults to an 8 MiB maximum frame.
        // Logical messages may exceed that default because each wire chunk is
        // independently bounded by our smaller, explicit MAX_FRAME_SIZE.
        let large = 8 * 1024 * 1024 + 1;
        for message in [
            SimulatorToSurferMessage::Hierarchy {
                hierarchy: vec![
                    HierarchyElement::PushScope {
                        name: "a".repeat(large),
                        scope_type: ScopeType::Module,
                    },
                    HierarchyElement::PopScope,
                ],
                exponent: -9,
            },
            SimulatorToSurferMessage::ValueChanges {
                time_steps: vec![TimeStep {
                    timestamp: 17,
                    changes: vec![SignalChange {
                        id: 5,
                        value: SignalValue::VCDValue(vec![b'1'; large]),
                    }],
                }],
                status: SimulationStatus::Done,
            },
        ] {
            let encoded = encode_simulator_to_surfer_message(&message).unwrap();
            assert!(encoded.len() > 8 * 1024 * 1024);
            let restored = roundtrip(&encoded);
            assert_eq!(restored, encoded);
            let decoded = decode_simulator_to_surfer_message(&restored).unwrap();
            assert_eq!(
                encode_simulator_to_surfer_message(&decoded).unwrap(),
                encoded
            );
        }
        for request in [
            SurferToSimulatorMessage::TrackVariableChanges {
                ids: vec![u32::MAX; 2 * 1024 * 1024],
            },
            SurferToSimulatorMessage::UntrackVariableChanges {
                ids: vec![u32::MAX; 2 * 1024 * 1024],
            },
        ] {
            let encoded = encode_surfer_to_simulator_message(&request).unwrap();
            assert!(encoded.len() > 8 * 1024 * 1024);
            let decoded = decode_surfer_to_simulator_message(&roundtrip(&encoded)).unwrap();
            assert_eq!(
                encode_surfer_to_simulator_message(&decoded).unwrap(),
                encoded
            );
        }
    }

    #[test]
    fn invalid_and_truncated_chunks_are_rejected() {
        let partial = TransportChunk {
            data: vec![1],
            last: false,
            protocol_version: PROTOCOL_VERSION,
        }
        .encode_to_vec();
        let valid = encode_chunks(&[2]).next().unwrap();
        let mut assembler = MessageAssembler::default();
        assert!(assembler.push(&partial).unwrap().is_none());
        assert!(assembler.finish().is_err());
        for invalid in [
            vec![],
            vec![0xff],
            vec![0; MAX_FRAME_SIZE + 1],
            TransportChunk {
                data: vec![1],
                last: true,
                protocol_version: 999,
            }
            .encode_to_vec(),
            TransportChunk {
                data: vec![],
                last: true,
                protocol_version: PROTOCOL_VERSION,
            }
            .encode_to_vec(),
        ] {
            assert!(assembler.push(&invalid).is_err());
            assert_eq!(assembler.push(&valid).unwrap(), Some(vec![2]));
        }
    }

    #[tokio::test]
    async fn framed_writer_matches_documented_wire_example() {
        use tokio::io::AsyncReadExt;
        let request =
            encode_surfer_to_simulator_message(&SurferToSimulatorMessage::RequestHierarchy)
                .unwrap();
        let (sender, mut receiver) = tokio::io::duplex(64);
        let mut writer = FramedWrite::new(
            sender,
            LengthDelimitedCodec::builder()
                .length_field_length(2)
                .max_frame_length(MAX_FRAME_SIZE)
                .new_codec(),
        );
        write_message(&mut writer, &request).await.unwrap();
        drop(writer);
        let mut wire = Vec::new();
        receiver.read_to_end(&mut wire).await.unwrap();
        assert_eq!(wire, [0, 8, 0x0a, 2, 0x22, 0, 0x10, 1, 0x18, 1]);
    }

    #[tokio::test]
    async fn framed_stream_preserves_message_order_across_partial_tcp_reads() {
        let (sender, receiver) = tokio::io::duplex(17);
        let first = vec![7; CHUNK_DATA_SIZE * 3 + 1];
        let second = vec![9; CHUNK_DATA_SIZE];
        let send = async {
            let mut writer = FramedWrite::new(
                sender,
                LengthDelimitedCodec::builder()
                    .length_field_length(2)
                    .max_frame_length(MAX_FRAME_SIZE)
                    .new_codec(),
            );
            write_message(&mut writer, &first).await.unwrap();
            write_message(&mut writer, &second).await.unwrap();
        };
        let receive = async {
            let mut reader = FramedRead::new(
                receiver,
                LengthDelimitedCodec::builder()
                    .length_field_length(2)
                    .max_frame_length(MAX_FRAME_SIZE)
                    .new_codec(),
            );
            let mut assembler = MessageAssembler::default();
            let mut messages = Vec::new();
            while let Some(frame) = reader.next().await {
                if let Some(message) = assembler.push(&frame.unwrap()).unwrap() {
                    messages.push(message);
                }
            }
            assembler.finish().unwrap();
            assert_eq!(messages, vec![first.clone(), second.clone()]);
        };
        tokio::join!(send, receive);
    }
}
