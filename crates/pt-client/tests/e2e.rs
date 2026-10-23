//! End-to-end: a fake plugin talks to the real listener over loopback, and the
//! real uploader sends everything to a fake server that fails at first.

use std::collections::BTreeMap;
use std::io::Read;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use axum::Router;
use pt_client::db::Db;
use pt_client::status::Status;
use pt_client::uploader::UploaderConfig;
use pt_core::config::Multiplayer;
use pt_core::upload::{ManifestEntry, UploadAck, UploadChunk};
use pt_core::wire::{encode_frame, FrameDecoder};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const S1: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const S0: &str = "0000000000000000000000000000000f"; // an older session replayed from the plugin journal
const TOKEN: &str = "player-token-123";
const RETRY: Duration = Duration::from_millis(300);
const FAIL_FIRST: usize = 2;

#[derive(Default)]
struct FakeServer {
    fail_first: usize,
    requests: Vec<Instant>,
    stored: BTreeMap<(String, u64), String>,
    chunks: Vec<(String, u64, u64, String, usize)>, // sess, from, to, config_hash, manifest entries
}

async fn chunks(State(srv): State<Arc<Mutex<FakeServer>>>, headers: HeaderMap, body: Bytes) -> (StatusCode, String) {
    let mut s = srv.lock().unwrap();
    s.requests.push(Instant::now());
    if s.requests.len() <= s.fail_first {
        return (StatusCode::SERVICE_UNAVAILABLE, "try later".into());
    }
    assert_eq!(headers["authorization"], format!("Bearer {TOKEN}"));
    assert_eq!(headers["content-encoding"], "gzip");
    let mut json = String::new();
    flate2::read::GzDecoder::new(&body[..]).read_to_string(&mut json).unwrap();
    let chunk: UploadChunk = serde_json::from_str(&json).unwrap();
    for f in &chunk.frames {
        let v: serde_json::Value = serde_json::from_str(f.get()).unwrap();
        let seq = v["seq"].as_u64().unwrap();
        s.stored.insert((chunk.sess.clone(), seq), f.get().to_string());
    }
    s.chunks.push((chunk.sess.clone(), chunk.seq_from, chunk.seq_to, chunk.config_hash.clone(), chunk.manifest.len()));
    let ack = UploadAck { sess: chunk.sess, accepted_through: chunk.seq_to };
    (StatusCode::OK, serde_json::to_string(&ack).unwrap())
}

fn frame(sess: &str, kind: &str, seq: Option<u64>, extra: &str) -> Vec<u8> {
    let json = match seq {
        Some(s) => format!("{{\"v\":1,\"kind\":\"{kind}\",\"sess\":\"{sess}\",\"seq\":{s},\"wall_ms\":1{extra}}}"),
        None => format!("{{\"v\":1,\"kind\":\"{kind}\",\"sess\":\"{sess}\",\"wall_ms\":1{extra}}}"),
    };
    encode_frame(&json)
}

/// Acks received from the client on one connection (highest seq per session).
#[derive(Default)]
struct Acks {
    dec: FrameDecoder,
    max: BTreeMap<String, u64>,
}

/// Reads acks from the client until the one for `sess` reaches `seq`.
async fn wait_ack(stream: &mut TcpStream, acks: &mut Acks, sess: &str, seq: u64) {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut buf = [0u8; 4096];
    loop {
        while let Some(f) = acks.dec.next_frame().unwrap() {
            let v: serde_json::Value = serde_json::from_str(&f).unwrap();
            assert_eq!(v["kind"], "ack");
            let e = acks.max.entry(v["sess"].as_str().unwrap().to_string()).or_default();
            *e = (*e).max(v["seq"].as_u64().unwrap());
        }
        if acks.max.get(sess).is_some_and(|s| *s >= seq) {
            return;
        }
        assert!(Instant::now() < deadline, "no ack for {sess} {seq}");
        let n = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut buf)).await.unwrap().unwrap();
        assert!(n > 0, "client closed the connection");
        acks.dec.push(&buf[..n]);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn plugin_to_client_to_server() {
    let dir = tempfile::tempdir().unwrap();
    let db = Arc::new(Mutex::new(Db::open(&dir.path().join("client.sqlite")).unwrap()));
    let status = Arc::new(Mutex::new(Status::default()));

    // Real listener on an ephemeral loopback port.
    let plugin_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let plugin_addr = plugin_listener.local_addr().unwrap();
    tokio::spawn(pt_client::listener::serve(plugin_listener, db.clone(), status.clone(), None));

    // Fake server.
    let srv = Arc::new(Mutex::new(FakeServer { fail_first: FAIL_FIRST, ..Default::default() }));
    let app = Router::new().route("/api/v1/chunks", post(chunks)).with_state(srv.clone());
    let server_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let server_url = format!("http://{}", server_listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(server_listener, app).await.unwrap() });

    // Real uploader.
    let cfg = pt_core::Config::from_toml_str(pt_client::paths::SAMPLE_CONFIG).unwrap();
    let ucfg = UploaderConfig {
        server_url,
        token: TOKEN.into(),
        retry_interval: RETRY,
        idle_poll: Duration::from_millis(50),
        batch_interval: Duration::ZERO,
        max_frames: 25,
        max_bytes: 1 << 20,
        multiplayer: Multiplayer { truckersmp_module_names: vec!["core_atsmp.dll".into()], mods: vec![] },
        config_hash: cfg.integrity_hash(),
        config_canonical_json: cfg.integrity_canonical_json(),
    };
    let manifest = Arc::new(Mutex::new(vec![ManifestEntry { role: "plugin".into(), path: "p".into(), sha256: "00".into() }]));
    tokio::spawn(pt_client::uploader::run(ucfg, db.clone(), status.clone(), manifest));

    // ---- fake plugin, first connection --------------------------------------
    let mut conn = TcpStream::connect(plugin_addr).await.unwrap();
    let mut acks = Acks::default();
    let mut out = Vec::new();
    out.extend(frame(S1, "hello", None, ",\"last_seq\":0"));
    out.extend(frame(S1, "snapshot", None, ",\"paused\":true,\"configs\":[],\"last_sample\":null"));
    out.extend(frame(S0, "shutdown", Some(900), "")); // replayed from the journal of an older session
    out.extend(frame(S1, "init", Some(1), ",\"game_id\":\"ats\""));
    out.extend(frame(S1, "modules", Some(2), ",\"modules\":[\"KERNEL32.DLL\",\"Core_ATSMP.dll\",\"private-mod.dll\"]"));
    out.extend(frame(S1, "config", Some(3), ",\"id\":\"job\",\"attrs\":[]"));
    for s in (4..=60).filter(|s| *s != 51) {
        out.extend(frame(S1, "sample", Some(s), ",\"ch\":{\"truck.speed\":20.5}"));
    }
    // Split writes at awkward boundaries.
    for piece in out.chunks(777) {
        conn.write_all(piece).await.unwrap();
    }
    wait_ack(&mut conn, &mut acks, S1, 60).await;
    wait_ack(&mut conn, &mut acks, S0, 900).await;
    drop(conn);

    // ---- reconnect: plugin replays 55..=60 (unacked from its view) + new frames --
    let mut conn = TcpStream::connect(plugin_addr).await.unwrap();
    let mut acks = Acks::default();
    let mut out = Vec::new();
    out.extend(frame(S1, "hello", None, ",\"last_seq\":60"));
    for s in 55..=70 {
        out.extend(frame(S1, "sample", Some(s), ",\"ch\":{\"truck.speed\":21}"));
    }
    conn.write_all(&out).await.unwrap();
    wait_ack(&mut conn, &mut acks, S1, 70).await;

    // Local log: no duplicates, the dropped seq stays missing, game recorded.
    {
        let db = db.lock().unwrap();
        let seqs = db.frame_seqs(S1).unwrap();
        let expected: Vec<u64> = (1..=70).filter(|s| *s != 51).collect();
        assert_eq!(seqs, expected);
        assert_eq!(db.session_game(S1).unwrap().as_deref(), Some("ats"));
    }

    // ---- server receives everything despite the initial failures -------------
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let done = {
            let s = srv.lock().unwrap();
            s.stored.len() == 69 + 1
        };
        if done && db.lock().unwrap().stats().unwrap().frames_pending_upload == 0 {
            break;
        }
        assert!(Instant::now() < deadline, "uploads did not complete");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let s = srv.lock().unwrap();
    // Fixed-interval retries after each failure.
    for w in s.requests.windows(2).take(FAIL_FIRST) {
        assert!(w[1] - w[0] >= RETRY - Duration::from_millis(20), "retry came after {:?}", w[1] - w[0]);
    }
    // Chunks respect max_frames and carry the config hash and manifest.
    assert!(s.chunks.iter().all(|c| c.2 - c.1 < 25 + 1 && c.3 == cfg.integrity_hash() && c.4 == 1));
    // Privacy: the module list never leaves the PC; only TruckersMP matches do.
    let modules = &s.stored[&(S1.to_string(), 2)];
    assert!(!modules.contains("private-mod.dll") && !modules.contains("KERNEL32"));
    assert!(modules.contains("\"truckersmp_matches\":[\"Core_ATSMP.dll\"]"));
    assert!(modules.contains("\"module_count\":3"));
    // Frames are byte-identical otherwise.
    assert!(s.stored[&(S1.to_string(), 70)].contains("\"truck.speed\":21"));
    assert!(s.stored.contains_key(&(S0.to_string(), 900)));

    let st = status.lock().unwrap();
    assert!(st.last_upload_ok_ms.is_some());
    assert!(st.frames_received >= 70);
}

/// Frames wait for the batch interval and go together; a game event (a
/// delivery) goes at once (owner, 2026-10-16).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn uploads_are_batched_but_deliveries_go_at_once() {
    const BATCH: Duration = Duration::from_millis(1500);
    let dir = tempfile::tempdir().unwrap();
    let db = Arc::new(Mutex::new(Db::open(&dir.path().join("client.sqlite")).unwrap()));
    let status = Arc::new(Mutex::new(Status::default()));
    let plugin_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let plugin_addr = plugin_listener.local_addr().unwrap();
    tokio::spawn(pt_client::listener::serve(plugin_listener, db.clone(), status.clone(), None));
    let srv = Arc::new(Mutex::new(FakeServer::default()));
    let app = Router::new().route("/api/v1/chunks", post(chunks)).with_state(srv.clone());
    let server_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let server_url = format!("http://{}", server_listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(server_listener, app).await.unwrap() });
    let cfg = pt_core::Config::from_toml_str(pt_client::paths::SAMPLE_CONFIG).unwrap();
    let ucfg = UploaderConfig {
        server_url,
        token: TOKEN.into(),
        retry_interval: RETRY,
        idle_poll: Duration::from_millis(50),
        batch_interval: BATCH,
        max_frames: 5000,
        max_bytes: 4 << 20,
        multiplayer: Multiplayer { truckersmp_module_names: vec![], mods: vec![] },
        config_hash: cfg.integrity_hash(),
        config_canonical_json: cfg.integrity_canonical_json(),
    };
    let manifest = Arc::new(Mutex::new(vec![]));
    tokio::spawn(pt_client::uploader::run(ucfg, db.clone(), status.clone(), manifest));

    let mut conn = TcpStream::connect(plugin_addr).await.unwrap();
    let mut acks = Acks::default();
    let start = Instant::now();
    let mut out = Vec::new();
    out.extend(frame(S1, "hello", None, ",\"last_seq\":0"));
    out.extend(frame(S1, "init", Some(1), ",\"game_id\":\"ats\""));
    // Samples trickling in, as while driving.
    for s in 2..=11 {
        out.extend(frame(S1, "sample", Some(s), ",\"ch\":{\"truck.speed\":20.5}"));
    }
    conn.write_all(&out).await.unwrap();
    wait_ack(&mut conn, &mut acks, S1, 11).await;
    for s in 12..=21 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        conn.write_all(&frame(S1, "sample", Some(s), ",\"ch\":{\"truck.speed\":20.5}")).await.unwrap();
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(srv.lock().unwrap().chunks.is_empty(), "sent before the batch interval");

    // Then one upload with all of them, about BATCH after the first.
    let deadline = Instant::now() + Duration::from_secs(10);
    while srv.lock().unwrap().chunks.is_empty() {
        assert!(Instant::now() < deadline, "the batch never went");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(start.elapsed() >= BATCH - Duration::from_millis(100), "{:?}", start.elapsed());
    {
        let s = srv.lock().unwrap();
        assert_eq!(s.chunks.len(), 1, "{:?}", s.chunks);
        assert_eq!((s.chunks[0].1, s.chunks[0].2), (1, 21));
    }

    // A delivery does not wait.
    let sent = Instant::now();
    conn.write_all(&frame(S1, "gameplay", Some(22), ",\"id\":\"job.delivered\",\"attrs\":[]")).await.unwrap();
    while srv.lock().unwrap().chunks.len() < 2 {
        assert!(sent.elapsed() < BATCH / 2, "the delivery waited for the batch");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(srv.lock().unwrap().chunks[1].2, 22);
}
