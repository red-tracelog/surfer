//! Native TCP session shared by simulator plugins. Simulator APIs stay on the callback thread.
use std::sync::OnceLock;

use futures::StreamExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};
use tokio_util::codec::{FramedRead, FramedWrite, LengthDelimitedCodec};

use super::{ClientConnection, SimulatorCommand};
use crate::{
    MAX_FRAME_SIZE, SimulatorToSurferMessage, SurferToSimulatorMessage,
    decode_surfer_to_simulator_message, encode_simulator_to_surfer_message,
    transport::{MessageAssembler, write_message},
};

/// Accept one Surfer session. The caller processes `commands` on the simulator thread.
/// Shutdown is acknowledged on the socket before `Finish` is queued.
/// Bind/accept failures also request simulator shutdown to release waiting callbacks.
pub async fn serve(
    bind_address: &str,
    initial_info: SimulatorToSurferMessage,
    connection: &OnceLock<ClientConnection>,
    commands: &mpsc::Sender<SimulatorCommand>,
    log: fn(String),
    debug: fn(String),
) {
    debug(format!("starting message listener on {bind_address}"));
    let listener = match TcpListener::bind(bind_address).await {
        Ok(listener) => listener,
        Err(err) => {
            log(format!("Failed to bind to {bind_address}: {err:?}"));
            request_simulator_finish(commands, log).await;
            return;
        }
    };
    let (stream, _) = match listener.accept().await {
        Ok(connection) => connection,
        Err(err) => {
            log(format!("Failed to accept simulator connection: {err:?}"));
            request_simulator_finish(commands, log).await;
            return;
        }
    };
    debug("accepted Surfer connection".into());
    handle_connection(stream, initial_info, connection, commands, log, debug).await;
}

async fn handle_connection(
    stream: TcpStream,
    initial_info: SimulatorToSurferMessage,
    connection: &OnceLock<ClientConnection>,
    commands: &mpsc::Sender<SimulatorCommand>,
    log: fn(String),
    debug: fn(String),
) {
    let (read_half, write_half) = stream.into_split();

    let mut reader = FramedRead::new(
        read_half,
        LengthDelimitedCodec::builder()
            .length_field_length(2)
            .max_frame_length(MAX_FRAME_SIZE)
            .new_codec(),
    );
    let mut writer = FramedWrite::new(
        write_half,
        LengthDelimitedCodec::builder()
            .length_field_length(2)
            .max_frame_length(MAX_FRAME_SIZE)
            .new_codec(),
    );

    // Proactively send simulator info once connected. This keeps startup robust
    // if the first inbound request frame is delayed or dropped by a proxy layer.
    match encode_simulator_to_surfer_message(&initial_info) {
        Ok(encoded) => {
            if let Err(err) = write_message(&mut writer, &encoded).await {
                log(format!(
                    "Failed to send initial SimulatorInfo to Surfer: {err:?}"
                ));
            }
        }
        Err(err) => {
            log(format!(
                "Failed to encode initial SimulatorInfo for Surfer: {err:?}"
            ));
        }
    }

    let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
    if connection.set(ClientConnection::new(tx)).is_err() {
        log("Client connection was already initialized".into());
        return;
    }

    let mut assembler = MessageAssembler::default();
    loop {
        tokio::select! {
            frame_result = reader.next() => {
                let frame = match frame_result {
                    Some(Ok(frame)) => frame,
                    Some(Err(err)) => {
                        log(format!("Transport error from Surfer: {err}"));
                        break;
                    }
                    None => {
                        if let Err(err) = assembler.finish() {
                            log(format!("Truncated message from Surfer: {err}"));
                        }
                        break;
                    }
                };

                let frame = match assembler.push(&frame) {
                    Ok(Some(message)) => message,
                    Ok(None) => continue,
                    Err(err) => {
                        log(format!("Invalid transport chunk from Surfer: {err}"));
                        break;
                    }
                };

                let msg: SurferToSimulatorMessage = match decode_surfer_to_simulator_message(&frame) {
                    Ok(msg) => msg,
                    Err(err) => {
                        log(format!("Failed to decode message from Surfer: {err:?}"));
                        continue;
                    }
                };

                debug(format!("received message from Surfer: {msg:?}"));

                let is_shutdown = matches!(&msg, SurferToSimulatorMessage::Shutdown);
                let response = dispatch_surfer_message(commands, msg).await;

                let encoded = match encode_simulator_to_surfer_message(&response) {
                    Ok(encoded) => encoded,
                    Err(err) => {
                        log(format!("Failed to serialize response to Surfer: {err:?}"));
                        continue;
                    }
                };
                if let Err(err) = write_message(&mut writer, &encoded).await {
                    log(format!("Failed to send response to Surfer: {err:?}"));
                    break;
                }

                if is_shutdown {
                    request_simulator_finish(commands, log).await;
                    break;
                }
            }

            Some(encoded) = rx.recv() => {
                debug(format!("sending asynchronous message to Surfer ({} bytes)", encoded.len()));
                if let Err(err) = write_message(&mut writer, &encoded).await {
                    log(format!("Failed to send plugin message to Surfer: {err:?}"));
                    break;
                }
            }
        }
    }
}

async fn dispatch_surfer_message(
    sender: &mpsc::Sender<SimulatorCommand>,
    message: SurferToSimulatorMessage,
) -> SimulatorToSurferMessage {
    let (response, response_receiver) = oneshot::channel();
    if sender
        .send(SimulatorCommand::Message { message, response })
        .await
        .is_err()
    {
        return SimulatorToSurferMessage::Acknowledge {
            success: false,
            error: Some("Failed to queue simulator command".into()),
        };
    }
    response_receiver
        .await
        .unwrap_or(SimulatorToSurferMessage::Acknowledge {
            success: false,
            error: Some("Simulator command response unavailable".into()),
        })
}

async fn request_simulator_finish(sender: &mpsc::Sender<SimulatorCommand>, log: fn(String)) {
    if sender.send(SimulatorCommand::Finish).await.is_err() {
        log("Failed to queue simulator shutdown".into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        HierarchyElement, PROTOCOL_VERSION, ScopeType, decode_simulator_to_surfer_message,
        encode_surfer_to_simulator_message,
    };
    use tokio::net::tcp::OwnedReadHalf;
    use tokio::time::{Duration, timeout};

    fn ignore_log(_: String) {}

    async fn receive(
        reader: &mut FramedRead<OwnedReadHalf, LengthDelimitedCodec>,
    ) -> SimulatorToSurferMessage {
        let mut assembler = MessageAssembler::default();
        loop {
            let frame = reader.next().await.unwrap().unwrap();
            if let Some(message) = assembler.push(&frame).unwrap() {
                return decode_simulator_to_surfer_message(&message).unwrap();
            }
        }
    }

    #[tokio::test]
    async fn session_dispatches_chunked_messages_updates_and_shutdown() {
        timeout(Duration::from_secs(5), async {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let client = TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
            let (server, _) = listener.accept().await.unwrap();
            let connection = OnceLock::new();
            let (commands, mut requests) = mpsc::channel(4);
            let initial_info = SimulatorToSurferMessage::SimulatorInfo {
                protocol_version: PROTOCOL_VERSION, name: "test".into(), version: "1".into(),
            };
            let session = handle_connection(server, initial_info, &connection, &commands, ignore_log, ignore_log);
            let backend = async {
                let Some(SimulatorCommand::Message { message, response }) = requests.recv().await else {
                    panic!("expected a simulator request");
                };
                assert!(matches!(message, SurferToSimulatorMessage::TrackVariableChanges { ids } if ids.len() == MAX_FRAME_SIZE));
                response.send(SimulatorToSurferMessage::Hierarchy {
                    hierarchy: vec![
                        HierarchyElement::PushScope { name: "a".repeat(MAX_FRAME_SIZE * 2), scope_type: ScopeType::Module },
                        HierarchyElement::PopScope,
                    ],
                    exponent: -9,
                }).unwrap();
                let Some(SimulatorCommand::Message { message, response }) = requests.recv().await else {
                    panic!("expected shutdown");
                };
                assert!(matches!(message, SurferToSimulatorMessage::Shutdown));
                response.send(SimulatorToSurferMessage::Acknowledge { success: true, error: None }).unwrap();
                assert!(matches!(requests.recv().await, Some(SimulatorCommand::Finish)));
            };
            let frontend = async {
                let (read, write) = client.into_split();
                let mut reader = FramedRead::new(read, LengthDelimitedCodec::builder().length_field_length(2).new_codec());
                let mut writer = FramedWrite::new(write, LengthDelimitedCodec::builder().length_field_length(2).new_codec());
                assert!(matches!(receive(&mut reader).await, SimulatorToSurferMessage::SimulatorInfo { name, .. } if name == "test"));
                // Malformed protobuf must not prevent decoding the next complete message.
                write_message(&mut writer, &[0xff]).await.unwrap();
                let request = encode_surfer_to_simulator_message(&SurferToSimulatorMessage::TrackVariableChanges { ids: vec![u32::MAX; MAX_FRAME_SIZE] }).unwrap();
                write_message(&mut writer, &request).await.unwrap();
                let SimulatorToSurferMessage::Hierarchy { hierarchy, exponent } = receive(&mut reader).await else {
                    panic!("expected hierarchy");
                };
                assert_eq!(exponent, -9);
                assert!(matches!(&hierarchy[0], HierarchyElement::PushScope { name, .. } if name.len() == MAX_FRAME_SIZE * 2));
                connection.get().unwrap().send(SimulatorToSurferMessage::CurrentSimulationTime { time: 42 });
                assert!(matches!(receive(&mut reader).await, SimulatorToSurferMessage::CurrentSimulationTime { time: 42 }));
                let shutdown = encode_surfer_to_simulator_message(&SurferToSimulatorMessage::Shutdown).unwrap();
                write_message(&mut writer, &shutdown).await.unwrap();
                assert!(matches!(receive(&mut reader).await, SimulatorToSurferMessage::Acknowledge { success: true, .. }));
                assert!(reader.next().await.is_none());
            };
            tokio::join!(session, backend, frontend);
        }).await.expect("session timed out");
    }

    #[tokio::test]
    async fn failed_bind_requests_simulator_finish() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (commands, mut requests) = mpsc::channel(1);
        let connection = OnceLock::new();
        let info = SimulatorToSurferMessage::SimulatorInfo {
            protocol_version: PROTOCOL_VERSION,
            name: "test".into(),
            version: "1".into(),
        };
        serve(
            &listener.local_addr().unwrap().to_string(),
            info,
            &connection,
            &commands,
            ignore_log,
            ignore_log,
        )
        .await;
        assert!(matches!(requests.try_recv(), Ok(SimulatorCommand::Finish)));
        assert!(connection.get().is_none());
    }

    #[tokio::test]
    async fn unavailable_command_queue_or_response_returns_failure() {
        let (sender, receiver) = mpsc::channel(1);
        drop(receiver);
        assert!(
            matches!(dispatch_surfer_message(&sender, SurferToSimulatorMessage::PauseSimulation).await,
            SimulatorToSurferMessage::Acknowledge { success: false, error: Some(error) } if error.contains("queue"))
        );

        let (sender, mut receiver) = mpsc::channel(1);
        let request = dispatch_surfer_message(&sender, SurferToSimulatorMessage::PauseSimulation);
        let backend = async {
            drop(receiver.recv().await.unwrap());
        };
        let (result, ()) = tokio::join!(request, backend);
        assert!(
            matches!(result, SimulatorToSurferMessage::Acknowledge { success: false, error: Some(error) } if error.contains("response"))
        );
    }
}
