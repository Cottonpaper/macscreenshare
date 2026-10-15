//! End-to-end test: a mock host speaks the real protocol and the client's
//! session logic receives its video.

use std::time::Duration;

use macss_client::args::Args;
use macss_client::session::SessionHooks;
use macss_protocol::{packetize_frame, Codec, ControlMessage, InputEvent, PROTOCOL_VERSION};
use macss_transport::{read_message, write_message, VideoSender};
use tokio::net::TcpListener;

#[tokio::test]
async fn client_receives_video_then_host_says_bye() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let host = tokio::spawn(async move {
        let (stream, peer) = listener.accept().await.unwrap();
        let (mut rd, mut wr) = stream.into_split();

        // Handshake.
        match read_message(&mut rd).await.unwrap().unwrap() {
            ControlMessage::Hello { protocol, .. } => assert_eq!(protocol, PROTOCOL_VERSION),
            other => panic!("expected hello, got {other:?}"),
        }
        write_message(
            &mut wr,
            &ControlMessage::HelloAck {
                protocol: PROTOCOL_VERSION,
                session_id: "test".into(),
                host_name: "mock-mac".into(),
                display_width: 1280,
                display_height: 720,
                scale: 2.0,
                video_port: 0,
                codecs: vec![Codec::H264],
            },
        )
        .await
        .unwrap();

        // Stream request tells us where to send video.
        let udp_port = match read_message(&mut rd).await.unwrap().unwrap() {
            ControlMessage::StartStream { udp_port, .. } => udp_port,
            other => panic!("expected start_stream, got {other:?}"),
        };

        let mut sender = VideoSender::connect((peer.ip(), udp_port).into())
            .await
            .unwrap();

        // Send a short burst of synthetic frames.
        for id in 0..20u32 {
            let payload: Vec<u8> = (0..3000u32).map(|i| (i % 251) as u8).collect();
            let datagrams = packetize_frame(id, Codec::H264, id == 0, id * 16, &payload);
            sender.send_datagrams(&datagrams).await.unwrap();
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        // Then close cleanly; the client should return Ok(()) on `bye`.
        write_message(&mut wr, &ControlMessage::Bye).await.unwrap();
        // Keep the connection alive briefly so the client reads the bye.
        tokio::time::sleep(Duration::from_millis(200)).await;
    });

    let args = Args {
        host: addr.ip().to_string(),
        control_port: addr.port(),
        ..Default::default()
    };

    let mut hooks = SessionHooks::default();
    let outcome = tokio::time::timeout(
        Duration::from_secs(10),
        macss_client::session::run_once(&args, &mut hooks),
    )
    .await
    .expect("session should finish before the timeout")
    .expect("session should end cleanly");

    assert_eq!(outcome, ());
    host.await.unwrap();
}

#[tokio::test]
async fn client_forwards_input_events_to_the_host() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let host = tokio::spawn(async move {
        let (stream, _peer) = listener.accept().await.unwrap();
        let (mut rd, mut wr) = stream.into_split();

        let _ = read_message(&mut rd).await.unwrap().unwrap();
        write_message(
            &mut wr,
            &ControlMessage::HelloAck {
                protocol: PROTOCOL_VERSION,
                session_id: "input".into(),
                host_name: "mock-mac".into(),
                display_width: 1280,
                display_height: 720,
                scale: 1.0,
                video_port: 0,
                codecs: vec![Codec::H264],
            },
        )
        .await
        .unwrap();
        let _ = read_message(&mut rd).await.unwrap().unwrap();

        let mut saw_input = false;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_millis(500), read_message(&mut rd)).await {
                Ok(Ok(Some(ControlMessage::Input {
                    event: InputEvent::MouseMove { x, y },
                }))) if (x - 0.25).abs() < 1e-6 && (y - 0.75).abs() < 1e-6 => {
                    saw_input = true;
                    break;
                }
                Ok(Ok(Some(_))) => {}
                _ => {}
            }
        }

        let _ = write_message(&mut wr, &ControlMessage::Bye).await;
        saw_input
    });

    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let _ = sender.send(InputEvent::MouseMove { x: 0.25, y: 0.75 });
    });

    let args = Args {
        host: addr.ip().to_string(),
        control_port: addr.port(),
        ..Default::default()
    };
    let mut hooks = SessionHooks {
        frames: None,
        input: Some(receiver),
    };

    let _ = tokio::time::timeout(
        Duration::from_secs(10),
        macss_client::session::run_once(&args, &mut hooks),
    )
    .await;

    assert!(
        host.await.unwrap(),
        "the host should have received the injected mouse move"
    );
}
