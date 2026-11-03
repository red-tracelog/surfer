use crate::{
    SystemState,
    message::Message,
    simulation::SimulatorInfo,
    spawn,
    time::time_string,
    wave_container::{SignalId, SimulationStatus, VariableRef, WaveContainer},
    wave_source::{LoadOptions, LoadProgress, LoadProgressStatus, WaveFormat, WaveSource},
    wellen::BodyResult,
};

use ahash::AHashMap;
#[cfg(all(target_arch = "wasm32", feature = "vscode"))]
use base64::{Engine as _, engine::general_purpose};
use egui::{Context, RichText};
use num::BigInt;
use std::sync::mpsc::Sender;
#[cfg(not(target_arch = "wasm32"))]
use surfer_simulator_protocol::MAX_FRAME_SIZE;
use surfer_simulator_protocol::transport::MessageAssembler;
#[cfg(all(target_arch = "wasm32", feature = "vscode"))]
use surfer_simulator_protocol::transport::encode_chunks;
#[cfg(not(target_arch = "wasm32"))]
use surfer_simulator_protocol::transport::write_message;
use surfer_simulator_protocol::{
    HierarchyElement, RunTime, ScopeType, SignalEncoding, SimId, SimulationDuration,
    SimulatorToSurferMessage, SurferToSimulatorMessage, TimeStep, VarDirection, VarIndex, VarType,
    decode_simulator_to_surfer_message, encode_surfer_to_simulator_message,
};
use surfer_time_types::{TimeScale, TimeUnit};
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
#[cfg(not(target_arch = "wasm32"))]
use tokio_stream::StreamExt;
#[cfg(not(target_arch = "wasm32"))]
use tokio_util::codec::{FramedRead, FramedWrite, LengthDelimitedCodec};
use tracing::{error, info, trace};
use wellen::{Hierarchy, HierarchyBuilder, SignalRef};

#[cfg(all(target_arch = "wasm32", feature = "vscode"))]
use crate::wasm_api::{
    clear_simulation_proxy_messages, next_simulation_proxy_message, notify_host,
};

impl SystemState {
    pub(crate) fn is_simulation(&self) -> bool {
        self.user
            .waves
            .as_ref()
            .is_some_and(|waves| matches!(waves.source, WaveSource::SimulatorPlugin))
    }

    pub(crate) fn draw_connect_to_simulator_dialog(
        &mut self,
        ctx: &Context,
        msgs: &mut Vec<Message>,
    ) {
        if !self.show_connect_to_simulator_dialog {
            return;
        }

        let mut should_close = false;
        let simulator_address = self
            .simulator_connection_address
            .get_or_insert_with(|| surfer_simulator_protocol::simulator_address(""));

        egui::Window::new("Connect to Running Simulator")
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label("Address or port");
                ui.text_edit_singleline(simulator_address);

                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("OK").clicked() {
                        msgs.push(Message::SimulatorPluginSetAddress(
                            simulator_address.clone(),
                        ));
                        msgs.push(Message::SimulatorPluginConnectTo);
                        should_close = true;
                    }

                    if ui.button("Cancel").clicked() {
                        should_close = true;
                    }
                });
            });

        if should_close {
            self.show_connect_to_simulator_dialog = false;
        }
    }
}

fn build_hierarchy(
    plugin_hierarchy: &[HierarchyElement],
    time_scale: &TimeScale,
) -> Result<
    (
        Hierarchy,
        AHashMap<u32, SignalRef>,
        AHashMap<SignalRef, u32>,
    ),
    surfer_simulator_protocol::ProtocolError,
> {
    surfer_simulator_protocol::validate_hierarchy_signal_encodings(plugin_hierarchy)?;
    let mut h = HierarchyBuilder::new(time_scale.into(), None, None);
    let mut id_to_signal_ref = AHashMap::new();
    let mut signal_ref_to_id = AHashMap::new();
    for elem in plugin_hierarchy {
        match elem {
            HierarchyElement::PushScope { name, scope_type } => {
                h.push_scope(name, scope_type_to_wellen(*scope_type), None);
            }
            HierarchyElement::PopScope => {
                h.pop_scope();
            }
            HierarchyElement::AddVar {
                id,
                name,
                type_name,
                var_type,
                signal_encoding,
                var_direction,
                enum_map,
                index,
                is_parameter,
            } => {
                let direction = var_direction
                    .map_or_else(wellen::VarDirection::vcd_default, var_direction_to_wellen);

                let signal_ref = *id_to_signal_ref.entry(*id).or_insert_with(|| {
                    let signal_ref = h.new_signal(signal_encoding_to_wellen(*signal_encoding));
                    signal_ref_to_id.insert(signal_ref, *id);
                    signal_ref
                });
                let mut enum_id = None;
                if let Some(enum_map) = enum_map {
                    let enum_map: Vec<(&str, &str)> = enum_map
                        .iter()
                        .map(|(option_name, display_value)| {
                            (option_name.as_str(), display_value.as_str())
                        })
                        .collect();
                    enum_id = Some(h.declare_enum(type_name, enum_map.iter().copied()));
                }

                h.add_var(
                    name,
                    var_type_to_wellen(*var_type, *is_parameter),
                    direction,
                    Some(type_name),
                    index.map(var_index_to_wellen),
                    enum_id,
                    signal_ref,
                );
            }
        }
    }

    Ok((h.finish(), id_to_signal_ref, signal_ref_to_id))
}

fn scope_type_to_wellen(scope_type: ScopeType) -> wellen::ScopeType {
    match scope_type {
        ScopeType::Unspecified | ScopeType::Unknown => wellen::ScopeType::Unknown,
        ScopeType::Module => wellen::ScopeType::Module,
        ScopeType::Task => wellen::ScopeType::Task,
        ScopeType::Function => wellen::ScopeType::Function,
        ScopeType::Begin => wellen::ScopeType::Begin,
        ScopeType::Fork => wellen::ScopeType::Fork,
        ScopeType::Generate => wellen::ScopeType::Generate,
        ScopeType::Struct => wellen::ScopeType::Struct,
        ScopeType::Union => wellen::ScopeType::Union,
        ScopeType::Class => wellen::ScopeType::Class,
        ScopeType::Interface => wellen::ScopeType::Interface,
        ScopeType::Package => wellen::ScopeType::Package,
        ScopeType::Program => wellen::ScopeType::Program,
        ScopeType::VhdlArchitecture => wellen::ScopeType::VhdlArchitecture,
        ScopeType::VhdlProcedure => wellen::ScopeType::VhdlProcedure,
        ScopeType::VhdlFunction => wellen::ScopeType::VhdlFunction,
        ScopeType::VhdlRecord => wellen::ScopeType::VhdlRecord,
        ScopeType::VhdlProcess => wellen::ScopeType::VhdlProcess,
        ScopeType::VhdlBlock => wellen::ScopeType::VhdlBlock,
        ScopeType::VhdlForGenerate => wellen::ScopeType::VhdlForGenerate,
        ScopeType::VhdlIfGenerate => wellen::ScopeType::VhdlIfGenerate,
        ScopeType::VhdlGenerate => wellen::ScopeType::VhdlGenerate,
        ScopeType::VhdlPackage => wellen::ScopeType::VhdlPackage,
        ScopeType::GhwGeneric => wellen::ScopeType::GhwGeneric,
        ScopeType::VhdlArray => wellen::ScopeType::VhdlArray,
        ScopeType::Clocking => wellen::ScopeType::Clocking,
        ScopeType::SvArray => wellen::ScopeType::SvArray,
    }
}

fn var_type_to_wellen(var_type: VarType, is_parameter: bool) -> wellen::VarType {
    if is_parameter {
        match var_type {
            VarType::Integer
            | VarType::Int
            | VarType::ShortInt
            | VarType::LongInt
            | VarType::Byte => return wellen::VarType::Parameter,
            VarType::Real | VarType::ShortReal => {
                return wellen::VarType::RealParameter;
            }
            VarType::Event => return wellen::VarType::EventParameter,
            _ => info!(
                "[simulation] Parameter type {var_type:?} is not supported yet; using its regular variable type"
            ),
        }
    }

    match var_type {
        VarType::Unspecified => wellen::VarType::Wire,
        VarType::Event => wellen::VarType::Event,
        VarType::Integer => wellen::VarType::Integer,
        VarType::Real => wellen::VarType::Real,
        VarType::Reg => wellen::VarType::Reg,
        VarType::Time => wellen::VarType::Time,
        VarType::Wire => wellen::VarType::Wire,
        VarType::String => wellen::VarType::String,
        VarType::Port => wellen::VarType::Port,
        VarType::Bit => wellen::VarType::Bit,
        VarType::Logic => wellen::VarType::Logic,
        VarType::Int => wellen::VarType::Int,
        VarType::ShortInt => wellen::VarType::ShortInt,
        VarType::LongInt => wellen::VarType::LongInt,
        VarType::Byte => wellen::VarType::Byte,
        VarType::Enum => wellen::VarType::Enum,
        VarType::ShortReal => wellen::VarType::ShortReal,
        VarType::Boolean => wellen::VarType::Boolean,
        VarType::BitVector => wellen::VarType::BitVector,
        VarType::StdLogic => wellen::VarType::StdLogic,
        VarType::StdLogicVector => wellen::VarType::StdLogicVector,
        VarType::StdULogic => wellen::VarType::StdULogic,
        VarType::StdULogicVector => wellen::VarType::StdULogicVector,
    }
}

fn var_direction_to_wellen(var_direction: VarDirection) -> wellen::VarDirection {
    match var_direction {
        VarDirection::Unspecified | VarDirection::Unknown => wellen::VarDirection::Unknown,
        VarDirection::Implicit => wellen::VarDirection::Implicit,
        VarDirection::Input => wellen::VarDirection::Input,
        VarDirection::Output => wellen::VarDirection::Output,
        VarDirection::InOut => wellen::VarDirection::InOut,
        VarDirection::Buffer => wellen::VarDirection::Buffer,
        VarDirection::Linkage => wellen::VarDirection::Linkage,
    }
}

fn signal_encoding_to_wellen(signal_encoding: SignalEncoding) -> wellen::SignalEncoding {
    match signal_encoding {
        SignalEncoding::String => wellen::SignalEncoding::String,
        SignalEncoding::Real => wellen::SignalEncoding::Real,
        SignalEncoding::BitVector(width) => wellen::SignalEncoding::BitVector(width),
    }
}

fn var_index_to_wellen(index: VarIndex) -> wellen::VarIndex {
    wellen::VarIndex::new(index.msb, index.lsb)
}

fn handle_simulator_response(response: SimulatorToSurferMessage, plugin_tx: &Sender<Message>) {
    match response {
        SimulatorToSurferMessage::SimulatorInfo {
            protocol_version,
            name,
            version,
        } => {
            let simulator_info = SimulatorInfo {
                protocol_version,
                name,
                version,
            };
            info!(
                "[simulation] Connected to simulator '{}' version '{}' (protocol v{})",
                simulator_info.name, simulator_info.version, simulator_info.protocol_version,
            );
            let _ = plugin_tx.send(Message::SimulatorPluginSetSimulatorInfo(Some(
                simulator_info,
            )));
            let _ = plugin_tx.send(Message::SimulatorPluginGetHierarchy);
        }
        SimulatorToSurferMessage::ValueChanges { time_steps, status } => {
            let _ = plugin_tx.send(Message::SimulatorPluginDataReceived(
                time_steps,
                status.into(),
            ));
        }
        SimulatorToSurferMessage::Hierarchy {
            hierarchy,
            exponent,
        } => {
            let _ = plugin_tx.send(Message::SimulatorPluginHierarchyReceived(
                hierarchy,
                protocol_exponent_to_internal(exponent),
            ));
        }
        SimulatorToSurferMessage::CurrentSimulationTime { time } => {
            let _ = plugin_tx.send(Message::SimulatorPluginTimeUpdated(time));
        }
        SimulatorToSurferMessage::Acknowledge { error, .. } => {
            if let Some(error) = error {
                error!("[simulation] Simulator command error: {error}");
            }
        }
    }
}

fn protocol_exponent_to_internal(exponent: i32) -> TimeScale {
    let unit_exponent = exponent.div_euclid(3) * 3;
    let Some(unit) = TimeUnit::from_exponent(unit_exponent as i8) else {
        return TimeScale::from_fs(1);
    };
    TimeScale {
        unit,
        multiplier: Some(10u32.pow((exponent - unit_exponent) as u32)),
    }
}

/// Function that listens to calls from the plugin, and send calls from surfer to the plugin.
#[cfg(not(target_arch = "wasm32"))]
async fn plugin_connection_loop(
    mut gui_rx: UnboundedReceiver<SurferToSimulatorMessage>,
    plugin_tx: Sender<Message>,
    bind_address: String,
) {
    info!("[simulation] connecting to {bind_address}");

    let address = bind_address;
    let max_wait = std::time::Duration::from_secs(20);
    let attempt_timeout = std::time::Duration::from_secs(2);
    let retry_delay = std::time::Duration::from_millis(100);
    let start_time = std::time::Instant::now();
    let mut stream = None;

    while start_time.elapsed() < max_wait {
        match tokio::time::timeout(attempt_timeout, tokio::net::TcpStream::connect(&address)).await
        {
            Ok(Ok(socket)) => {
                stream = Some(socket);
                break;
            }
            Ok(Err(e)) => {
                tracing::error!("[simulation] connect attempt failed: {e}");
            }
            Err(_) => {
                tracing::trace!("[simulation] waiting for plugin on {address}");
            }
        }
        tokio::time::sleep(retry_delay).await;
    }

    let Some(stream) = stream else {
        error!("[simulation] timed out waiting for {address}");
        let _ = plugin_tx.send(Message::SimulatorPluginConnectionClosed);
        return;
    };

    let (read_half, write_half) = stream.into_split();
    let mut writer = FramedWrite::new(
        write_half,
        LengthDelimitedCodec::builder()
            .length_field_length(2)
            .max_frame_length(MAX_FRAME_SIZE)
            .new_codec(),
    );
    let mut reader = FramedRead::new(
        read_half,
        LengthDelimitedCodec::builder()
            .length_field_length(2)
            .max_frame_length(MAX_FRAME_SIZE)
            .new_codec(),
    );

    info!("[simulation] Connected successfully! Starting listening loop.");

    let mut assembler = MessageAssembler::default();
    let mut connection_lost = false;
    loop {
        tokio::select! {
            //Checks if Surfer has any messages to send to the plugin
            msg_opt = gui_rx.recv() => {
                if let Some(msg) = msg_opt {
                    let encoded = match encode_surfer_to_simulator_message(&msg) {
                        Ok(encoded) => encoded,
                        Err(e) => {
                            error!("Failed to encode message to simulator: {}", e);
                            continue;
                        }
                    };
                    if let Err(e) = write_message(&mut writer, &encoded).await { //Sends the message if nothing goes wrong
                        error!("Failed to send to plugin: {}", e);
                        connection_lost = true;
                        break;
                    }
                } else {
                    //If the Surfer side has ended the connection
                    info!("[simulation] Channel closed. Shutting down plugin connection.");
                    break;
                }
            }

            frame_opt = reader.next() => { // Checks if a message from the plugin has arrived and sorts it correctly
                match frame_opt {
                    Some(Ok(frame)) => {
                        let frame = match assembler.push(&frame) {
                            Ok(Some(message)) => message,
                            Ok(None) => continue,
                            Err(e) => {
                                error!("Invalid transport chunk from plugin: {e}");
                                connection_lost = true;
                                break;
                            }
                        };
                        let response: surfer_simulator_protocol::SimulatorToSurferMessage =
                            match decode_simulator_to_surfer_message(&frame) {
                                Ok(message) => message,
                                Err(e) => {
                                    error!("Failed to decode message from plugin: {}", e);
                                    continue;
                                }
                            };

                        handle_simulator_response(response, &plugin_tx);
                    }
                    Some(Err(e)) => { // If we get an error when trying to read a message
                        error!("[simulation] TCP connection error: {}", e);
                        connection_lost = true;
                        break;
                    }
                    None => { // If the TCP-Stream has been closed
                        if let Err(e) = assembler.finish() {
                            error!("[simulation] Truncated message from plugin: {e}");
                        }
                        info!("[simulation] TCP stream closed. Shutting down plugin connection.");
                        connection_lost = true;
                        break;
                    }
                }
            }
        }
    }

    if connection_lost {
        let _ = plugin_tx.send(Message::SimulatorPluginConnectionClosed);
    }
}

#[cfg(all(target_arch = "wasm32", feature = "vscode"))]
async fn plugin_connection_loop(
    mut gui_rx: UnboundedReceiver<SurferToSimulatorMessage>,
    plugin_tx: Sender<Message>,
    bind_address: String,
) {
    info!("[simulation] VS Code plugin connection loop started");
    clear_simulation_proxy_messages().await;
    let start_message = serde_json::json!({
        "command": "simulationProxyStart",
        "bindAddress": bind_address,
    });
    notify_host(&start_message.to_string());
    let mut assembler = MessageAssembler::default();

    loop {
        tokio::select! {
            msg_opt = gui_rx.recv() => {
                match msg_opt {
                    Some(msg) => {
                        info!("[simulation] Received message from GUI: {:?}", msg);
                        let encoded = match encode_surfer_to_simulator_message(&msg) {
                            Ok(encoded) => encoded,
                            Err(e) => {
                                error!("Failed to encode message to simulator proxy: {}", e);
                                continue;
                            }
                        };
                        for chunk in encode_chunks(&encoded) {
                            let send_message = serde_json::json!({
                                "command": "simulationProxySend",
                                "frame": general_purpose::STANDARD.encode(chunk),
                            });
                            notify_host(&send_message.to_string());
                        }
                    }
                    None => {
                        let stop_message = serde_json::json!({ "command": "simulationProxyStop" });
                        notify_host(&stop_message.to_string());
                        break;
                    }
                }
            }

            frame = next_simulation_proxy_message() => {
                // The bridge uses an empty string as its connection-close marker.
                if frame.is_empty() {
                    if let Err(e) = assembler.finish() {
                        error!("Truncated message from simulation proxy: {e}");
                    }
                    let _ = plugin_tx.send(Message::SimulatorPluginConnectionClosed);
                    break;
                }
                let decoded_bytes = match general_purpose::STANDARD.decode(frame) {
                    Ok(bytes) => bytes,
                    Err(e) => {
                        error!("Failed to decode simulation proxy frame: {}", e);
                        notify_host(&serde_json::json!({ "command": "simulationProxyStop" }).to_string());
                        let _ = plugin_tx.send(Message::SimulatorPluginConnectionClosed);
                        break;
                    }
                };
                let decoded_bytes = match assembler.push(&decoded_bytes) {
                    Ok(Some(message)) => message,
                    Ok(None) => continue,
                    Err(e) => {
                        error!("Invalid transport chunk from simulation proxy: {e}");
                        notify_host(&serde_json::json!({ "command": "simulationProxyStop" }).to_string());
                        let _ = plugin_tx.send(Message::SimulatorPluginConnectionClosed);
                        break;
                    }
                };
                let response: SimulatorToSurferMessage = match decode_simulator_to_surfer_message(&decoded_bytes) {
                    Ok(message) => message,
                    Err(e) => {
                        error!("Failed to decode message from simulation proxy: {}", e);
                        continue;
                    }
                };

                handle_simulator_response(response, &plugin_tx);
            }
        }
    }
}

impl SystemState {
    pub(crate) fn draw_simulation_running(&self, ctx: &Context, msgs: &mut Vec<Message>) {
        egui::Window::new("Simulation is running")
            .anchor(egui::Align2::LEFT_CENTER, egui::vec2(-20.0, -20.0))
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    if let Some(t) = self.user.plugin_sim_time {
                        let sim_time = if let Some(waves) = self.user.waves.as_ref() {
                            let t_bigint = BigInt::from(t);
                            time_string(
                                &t_bigint,
                                &waves.inner.metadata().timescale,
                                &self.user.wanted_timeunit,
                                &self.get_time_format(),
                            )
                        } else {
                            t.to_string()
                        };

                        ui.label(RichText::new(format!("Simulation time: {sim_time}")).strong());
                    } else {
                        ui.label("Waiting for time from plugin...");
                    }

                    ui.add_space(10.0);

                    if ui.button("Pause simulation").clicked() {
                        msgs.push(Message::SimulatorPluginPause);
                        msgs.push(Message::PauseSimulation);
                    }
                });
            });
    }

    pub(crate) fn connect_to_simulator_plugin(&mut self) {
        // Sets up the TCP connection
        let (tcp_tx, tcp_rx) = unbounded_channel();
        self.channels.simulator_plugin_tx = Some(tcp_tx);

        let plugin_msg_tx = self.channels.simulator_plugin_msg_sender.clone();
        let bind_address = surfer_simulator_protocol::simulator_address(
            self.simulator_connection_address
                .as_deref()
                .unwrap_or_default(),
        );

        spawn!(plugin_connection_loop(tcp_rx, plugin_msg_tx, bind_address,));
    }

    pub(crate) fn simulator_plugin_hierarchy_received(
        &mut self,
        plugin_hierarchy: Vec<HierarchyElement>,
        time_scale: TimeScale,
    ) {
        //This runs when Surfer gets a hierarchy from the plugin. Sends it to wellen correctly
        let (hierarchy, id_to_signal_ref, signal_ref_to_id) =
            match build_hierarchy(&plugin_hierarchy, &time_scale) {
                Ok(hierarchy) => hierarchy,
                Err(err) => {
                    error!("Invalid simulator hierarchy: {err}");
                    return;
                }
            };
        self.signal_ref_to_id = Some(signal_ref_to_id);
        let new_waves = Box::new(WaveContainer::new_simulator_waveform(
            hierarchy.into(),
            id_to_signal_ref,
        ));
        self.on_waves_loaded(
            WaveSource::SimulatorPlugin,
            WaveFormat::Simulation,
            new_waves,
            LoadOptions::Clear,
        );

        if let Some(waves) = self.user.waves.as_mut()
            && let Some(container) = waves.inner.as_waves_mut()
        {
            container.set_simulation_status(SimulationStatus::Paused);
        }
    }

    pub(crate) fn simulator_plugin_data_received(
        &mut self,
        time_steps: Vec<TimeStep>,
        status: SimulationStatus,
    ) {
        if !self.is_simulation() {
            trace!("[simulation] Discarding value changes because no simulator waveform is active");
            return;
        }

        let Some(container) = self
            .user
            .waves
            .as_mut()
            .and_then(|waves| waves.inner.as_waves_mut())
        else {
            trace!("[simulation] Discarding value changes because no Wellen waveform is available");
            return;
        };

        if time_steps.is_empty() {
            container.set_simulation_status(status);
            if status != SimulationStatus::Running {
                self.progress_tracker = None;
            }
            if let Some(ctx) = &self.context {
                ctx.request_repaint();
            }
            return;
        }

        let body_result = container.wellen_add_signals_from_simulation(&time_steps, status);

        match body_result {
            Ok(body) => {
                self.update(Message::WaveBodyLoaded(
                    web_time::Instant::now(),
                    WaveSource::SimulatorPlugin,
                    BodyResult::Local(body),
                    Some(status),
                ));
            }
            Err(err) => error!("Failed to convert plugin values: {:?}", err),
        }
    }

    /// Tell simulator to track specified signals
    pub(crate) fn simulator_track_variables(&mut self, vars: &[VariableRef]) {
        if vars.is_empty() || !self.is_simulation() {
            return;
        }
        let mut ids = self.variable_refs_to_sim_ids(vars);
        ids.sort_unstable();
        ids.dedup();
        if ids.is_empty() {
            return;
        }
        if let Some(tx) = &self.channels.simulator_plugin_tx {
            let msg = SurferToSimulatorMessage::TrackVariableChanges { ids: ids.clone() };
            if let Err(e) = tx.send(msg) {
                error!("Failed to send track variables message to simulator: {}", e);
            }
        }
    }

    /// Tell simulator to untrack specified signals
    pub(crate) fn simulator_untrack_variables(&mut self, ids: &[SimId]) {
        if ids.is_empty() || !self.is_simulation() {
            return;
        }
        // Removes callbacks from specified signals
        if let Some(tx) = &self.channels.simulator_plugin_tx {
            let msg = SurferToSimulatorMessage::UntrackVariableChanges { ids: ids.to_vec() };
            if let Err(e) = tx.send(msg) {
                error!(
                    "Failed to send untrack variables message to simulator: {}",
                    e
                );
            }
        }
    }

    pub(crate) fn simulator_run_for_duration(&mut self, duration: Option<u64>) {
        if !self.is_simulation() {
            return;
        }
        if let Some(tx) = &self.channels.simulator_plugin_tx {
            self.progress_tracker = Some(LoadProgress::new(LoadProgressStatus::Simulating));
            let msg = SurferToSimulatorMessage::RunSimulation {
                time: duration
                    .map(|ticks| RunTime::Relative(SimulationDuration::from_ticks(ticks))),
            };
            if let Err(e) = tx.send(msg) {
                error!("[simulation] Failed to queue RunSimulation to plugin channel: {e}");
            } else if let Some(waves) = self.user.waves.as_mut()
                && let Some(container) = waves.inner.as_waves_mut()
            {
                container.set_simulation_status(SimulationStatus::Running);
            }
        } else {
            error!("Not connected to plugin! (Run ConnectToPlugin first)");
        }
        if let Some(ctx) = &self.context {
            ctx.request_repaint();
        }
    }

    #[cfg(any(feature = "vscode", not(target_arch = "wasm32")))]
    fn variable_refs_to_sim_ids(&self, vars: &[VariableRef]) -> Vec<SimId> {
        let container = self
            .user
            .waves
            .as_ref()
            .and_then(|waves| waves.inner.as_waves());
        container
            .map(|container| {
                vars.iter()
                    .filter_map(|var| {
                        if let Ok(SignalId::Wellen(signal_ref)) = container.signal_id(var)
                            && let Some(id) = self
                                .signal_ref_to_id
                                .as_ref()
                                .and_then(|map| map.get(&signal_ref))
                                .copied()
                        {
                            Some(id)
                        } else {
                            tracing::info!("Couldn't finds a Wellen-ID for: {}", var.name);
                            None
                        }
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    #[cfg(not(any(feature = "vscode", not(target_arch = "wasm32"))))]
    pub(crate) fn variable_refs_to_sim_ids(&self, vars: &[VariableRef]) -> Vec<SimId> {
        let _ = vars;
        vec![]
    }
}

#[cfg(test)]
mod alias_tests {
    use super::*;
    use crate::displayed_item::{DisplayedItem, DisplayedItemRef, DisplayedVariable};
    use crate::wave_container::VariableRefExt;

    #[test]
    fn renamed_aliases_share_storage_and_remain_tracked_until_last_instance() {
        let variable = |id, name: &str| HierarchyElement::AddVar {
            id,
            name: name.into(),
            index: None,
            type_name: "logic".into(),
            var_type: VarType::Logic,
            signal_encoding: SignalEncoding::BitVector(1),
            var_direction: None,
            enum_map: None,
            is_parameter: false,
        };
        let elements = vec![
            HierarchyElement::PushScope {
                name: "top".into(),
                scope_type: ScopeType::Module,
            },
            variable(7, "clk"),
            variable(8, "independent"),
            HierarchyElement::PushScope {
                name: "cpu".into(),
                scope_type: ScopeType::Module,
            },
            variable(7, "clock"),
            HierarchyElement::PopScope,
            HierarchyElement::PopScope,
        ];
        let (hierarchy, ids, reverse) = build_hierarchy(&elements, &TimeScale::from_fs(1)).unwrap();
        let clk = hierarchy.lookup_var(&["top"], "clk").unwrap();
        let alias = hierarchy.lookup_var(&["top", "cpu"], "clock").unwrap();
        let independent = hierarchy.lookup_var(&["top"], "independent").unwrap();
        assert_ne!(clk, alias);
        assert_eq!(hierarchy[clk].signal_ref(), hierarchy[alias].signal_ref());
        assert_ne!(
            hierarchy[clk].signal_ref(),
            hierarchy[independent].signal_ref()
        );
        assert_eq!(ids.len(), 2);
        assert_eq!(reverse.len(), 2);
        let container = WaveContainer::new_simulator_waveform(hierarchy.into(), ids);
        let displayed = |path: &[&str]| {
            DisplayedItem::Variable(DisplayedVariable {
                variable_ref: VariableRef::from_strs(path),
                info: Default::default(),
                color: None,
                background_color: None,
                display_name: path.last().unwrap().to_string(),
                display_name_type: crate::variable_name_type::VariableNameType::Local,
                manual_name: None,
                format: None,
                field_formats: Vec::new(),
                height_scaling_factor: None,
                analog: None,
                unfolded_fields: Default::default(),
            })
        };
        let first = DisplayedItemRef(0);
        let second = DisplayedItemRef(1);
        let mut items = std::collections::HashMap::from([
            (first, displayed(&["top", "clk"])),
            (second, displayed(&["top", "cpu", "clock"])),
        ]);
        let untrack_id = |item, items: &std::collections::HashMap<_, _>| {
            crate::simulation::displayed_item_ref_to_sim_id_if_last_instance(
                item,
                items,
                Some(&container),
                Some(&reverse),
                true,
            )
        };
        assert_eq!(untrack_id(first, &items), None);
        items.remove(&first);
        assert_eq!(untrack_id(second, &items), Some(7));
    }
}
