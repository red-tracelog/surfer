use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use reqwest::StatusCode;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

mod common;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_end_to_end_basic() {
    // Arrange: choose a free port
    let port = {
        let sock = std::net::TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
            .expect("bind 0");
        sock.local_addr().unwrap().port()
    };

    // Build a path to a small example waveform file in the workspace
    let mut file = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    file.push("..");
    file.push("examples");
    file.push("with_1_bit.vcd");
    let file = file.canonicalize().expect("example file canonicalize");

    let token = "testtoken123456".to_string(); // >= MIN_TOKEN_LEN

    let started = Arc::new(AtomicBool::new(false));
    let started_clone = started.clone();

    // Start server in the background
    let token_clone = token.clone();
    let handle = tokio::spawn(async move {
        if let Err(err) = surver::surver_main(
            port,
            "127.0.0.1".to_string(),
            Some(token_clone),
            Some("127.0.0.1:0".to_string()),
            &[file.to_string_lossy().to_string()],
            Some(started_clone),
        )
        .await
        {
            eprintln!("server_main error: {err:?}");
        }
    });

    // Wait until the server reports started (max ~5s)
    let mut waited_ms = 0;
    while !started.load(Ordering::SeqCst) && waited_ms < 5000 {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        waited_ms += 50;
    }
    assert!(
        started.load(Ordering::SeqCst),
        "server did not start in time"
    );

    let base = format!("http://127.0.0.1:{port}/{token}");
    let client = common::http_client();

    // 1) Invalid token should 404
    let resp = client
        .get(format!("http://127.0.0.1:{port}/invalidtoken"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    // 2) Info page should be 200 and include default headers
    let resp = client.get(base.clone()).send().await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let server_hdr = resp.headers().get(surver::HTTP_SERVER_KEY).cloned();
    assert!(server_hdr.is_some());
    assert_eq!(
        server_hdr.unwrap().to_str().unwrap(),
        surver::HTTP_SERVER_VALUE_SURFER
    );
    assert_eq!(
        resp.headers().get("Access-Control-Allow-Origin").unwrap(),
        "*"
    );
    assert!(
        resp.headers()
            .get("Access-Control-Expose-Headers")
            .unwrap()
            .to_str()
            .unwrap()
            .contains(surver::X_WELLEN_VERSION)
    );
    let body = resp.text().await.unwrap();
    assert!(body.contains("Surfer Remote Server"));

    // 3) Status endpoint
    let resp = client
        .get(format!("{base}/get_status"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get("Content-Type")
            .unwrap()
            .to_str()
            .unwrap(),
        "application/json"
    );
    let body = resp.text().await.unwrap();
    let status = serde_json::from_str::<surver::SurverStatus>(&body).unwrap();
    let file_info = &status.file_infos[0];
    assert!(file_info.bytes >= file_info.bytes_loaded);

    // The dynamically assigned WCP address is discoverable by trusted clients.
    let resp = client
        .get(format!("{base}/wcp/address"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.text().await.unwrap();
    let wcp_endpoint = serde_json::from_str::<serde_json::Value>(&body).unwrap();
    assert_eq!(wcp_endpoint["host"], "127.0.0.1");
    assert_ne!(wcp_endpoint["port"], 0);

    // 4) Hierarchy endpoint (lz4-compressed bincode), just check non-empty
    let resp = client
        .get(format!("{base}/0/get_hierarchy"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.bytes().await.unwrap();
    assert!(!bytes.is_empty());

    // 5) Timetable endpoint (waits until loaded)
    let resp = client
        .get(format!("{base}/0/get_time_table"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.bytes().await.unwrap();
    assert!(!bytes.is_empty());

    // 6) Signals endpoint with no IDs -> empty body
    let resp = client
        .get(format!("{base}/0/get_signals"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.bytes().await.unwrap();
    assert!(bytes.is_empty());

    // Cleanup: abort server task
    handle.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_loads_multiple_files() {
    // Arrange: choose a free port
    let port = {
        let sock = std::net::TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
            .expect("bind 0");
        sock.local_addr().unwrap().port()
    };

    // Build paths to multiple example waveform files
    let mut file1 = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    file1.push("..");
    file1.push("examples");
    file1.push("with_1_bit.vcd");
    let file1 = file1.canonicalize().expect("example file1 canonicalize");

    let mut file2 = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    file2.push("..");
    file2.push("examples");
    file2.push("with_8_bit.vcd");
    let file2 = file2.canonicalize().expect("example file2 canonicalize");

    let token = "testtoken789012".to_string();

    let started = Arc::new(AtomicBool::new(false));
    let started_clone = started.clone();

    // Start server with multiple files
    let token_clone = token.clone();
    let handle = tokio::spawn(async move {
        if let Err(err) = surver::surver_main(
            port,
            "127.0.0.1".to_string(),
            Some(token_clone),
            None,
            &[
                file1.to_string_lossy().to_string(),
                file2.to_string_lossy().to_string(),
            ],
            Some(started_clone),
        )
        .await
        {
            eprintln!("server_main error: {err:?}");
        }
    });

    // Wait for server startup
    let mut waited_ms = 0;
    while !started.load(Ordering::SeqCst) && waited_ms < 5000 {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        waited_ms += 50;
    }
    assert!(started.load(Ordering::SeqCst), "server did not start");

    let base = format!("http://127.0.0.1:{port}/{token}");
    let client = common::http_client();

    // Get status to verify both files are loaded
    let resp = client
        .get(format!("{base}/get_status"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = resp.text().await.unwrap();
    let status = serde_json::from_str::<surver::SurverStatus>(&body).unwrap();

    // Verify both files are in the file list
    assert_eq!(status.file_infos.len(), 2, "expected 2 files in the list");
    assert!(
        status.file_infos[0].bytes > 0,
        "first file should have content"
    );
    assert!(
        status.file_infos[1].bytes > 0,
        "second file should have content"
    );

    // Verify hierarchy endpoints for both files exist
    for file_idx in 0..2 {
        let resp = client
            .get(format!("{base}/{file_idx}/get_hierarchy"))
            .send()
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "hierarchy for file {file_idx} failed"
        );
        let bytes = resp.bytes().await.unwrap();
        assert!(
            !bytes.is_empty(),
            "hierarchy data for file {file_idx} is empty"
        );
    }

    // Verify time table for both files
    for file_idx in 0..2 {
        let resp = client
            .get(format!("{base}/{file_idx}/get_time_table"))
            .send()
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "timetable for file {file_idx} failed"
        );
        let bytes = resp.bytes().await.unwrap();
        assert!(!bytes.is_empty(), "timetable for file {file_idx} is empty");
    }

    // Cleanup
    handle.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wcp_relay_forwards_messages_in_both_directions() {
    let http_port = {
        let socket = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        socket.local_addr().unwrap().port()
    };
    let wcp_port = {
        let socket = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        socket.local_addr().unwrap().port()
    };

    let file = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../examples/with_1_bit.vcd")
        .canonicalize()
        .unwrap();
    let waveform_path = file.to_string_lossy().to_string();
    let server_waveform_path = waveform_path.clone();
    let token = "wcp-relay-test-token";
    let started = Arc::new(AtomicBool::new(false));
    let started_copy = started.clone();
    let handle = tokio::spawn(async move {
        surver::surver_main(
            http_port,
            Ipv4Addr::LOCALHOST.to_string(),
            Some(token.to_string()),
            Some(format!("127.0.0.1:{wcp_port}")),
            &[server_waveform_path],
            Some(started_copy),
        )
        .await
        .unwrap();
    });

    while !started.load(Ordering::SeqCst) {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    let base = format!("http://127.0.0.1:{http_port}/{token}/wcp");
    let client = common::http_client();
    let viewer_url = "https://forwarded.example/surver-token";
    let response = client
        .post(format!("{base}/viewer"))
        .body(viewer_url)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);

    let server_message = r#"{"type":"greeting","version":"0","commands":["load"]}"#;
    let response = client
        .post(format!("{base}/sc"))
        .body(server_message)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);

    // The browser-side greeting can arrive before the native WCP client connects.
    let mut wcp = tokio::net::TcpStream::connect((Ipv4Addr::LOCALHOST, wcp_port))
        .await
        .unwrap();
    let client_message = r#"{"type":"greeting","version":"0","commands":[]}"#;
    wcp.write_all(client_message.as_bytes()).await.unwrap();
    wcp.write_all(&[0]).await.unwrap();

    let response = client.get(format!("{base}/cs")).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.text().await.unwrap(), client_message);

    let mut received = Vec::new();
    loop {
        let mut buffer = [0_u8; 256];
        let count = wcp.read(&mut buffer).await.unwrap();
        assert!(count > 0);
        received.extend_from_slice(&buffer[..count]);
        if received.contains(&0) {
            break;
        }
    }
    assert_eq!(
        received.strip_suffix(&[0]).unwrap(),
        server_message.as_bytes()
    );

    let load_message = serde_json::json!({
        "type": "command",
        "command": "load",
        "source": waveform_path,
    });
    wcp.write_all(load_message.to_string().as_bytes())
        .await
        .unwrap();
    wcp.write_all(&[0]).await.unwrap();
    let response = client.get(format!("{base}/cs")).send().await.unwrap();
    let forwarded_load =
        serde_json::from_str::<serde_json::Value>(&response.text().await.unwrap()).unwrap();
    assert_eq!(forwarded_load["source"], viewer_url);

    let loaded_event = serde_json::json!({
        "type": "event",
        "event": "waveforms_loaded",
        "source": viewer_url,
    });
    client
        .post(format!("{base}/sc"))
        .body(loaded_event.to_string())
        .send()
        .await
        .unwrap();
    let mut received = Vec::new();
    loop {
        let mut buffer = [0_u8; 256];
        let count = wcp.read(&mut buffer).await.unwrap();
        assert!(count > 0);
        received.extend_from_slice(&buffer[..count]);
        if received.contains(&0) {
            break;
        }
    }
    let loaded_event =
        serde_json::from_slice::<serde_json::Value>(received.strip_suffix(&[0]).unwrap()).unwrap();
    assert_eq!(loaded_event["source"], load_message["source"]);

    handle.abort();
}
