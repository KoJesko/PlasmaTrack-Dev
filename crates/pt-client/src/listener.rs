//! Accepts plugin connections on loopback (the plugin connects out to us) and
//! persists every frame before acknowledging it (docs/WIRE.md).
//!
//! Several connections may be open at once (e.g. ETS2 and ATS running together);
//! each carries its own plugin session.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use pt_core::wire::{ack_json, encode_frame, Envelope, FrameDecoder};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::db::{Db, Incoming};
use crate::live::Feed;
use crate::log;
use crate::status::SharedStatus;

/// Frames are committed and acknowledged at least this often, or sooner when a
/// batch reaches [`MAX_BATCH`].
pub const FLUSH_INTERVAL: Duration = Duration::from_millis(200);
pub const MAX_BATCH: usize = 1000;

/// `feed` gets every newly stored frame for the live results.
pub async fn serve(listener: TcpListener, db: Arc<Mutex<Db>>, status: SharedStatus, feed: Option<Feed>) {
    loop {
        match listener.accept().await {
            Ok((stream, peer)) => {
                if !peer.ip().is_loopback() {
                    continue; // bound to loopback anyway; defence in depth
                }
                let db = db.clone();
                let status = status.clone();
                let feed = feed.clone();
                tokio::spawn(async move {
                    status.lock().expect("status").plugin_connections += 1;
                    log!("plugin connected from {peer}");
                    if let Err(e) = handle(stream, db, status.clone(), feed).await {
                        log!("plugin connection error: {e}");
                    }
                    status.lock().expect("status").plugin_connections -= 1;
                    log!("plugin disconnected");
                });
            }
            Err(e) => {
                log!("accept failed: {e}");
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    }
}

async fn flush(
    pending: &mut Vec<Incoming>,
    db: &Arc<Mutex<Db>>,
    status: &SharedStatus,
    feed: &Option<Feed>,
    writer: &mut tokio::net::tcp::OwnedWriteHalf,
) -> std::io::Result<()> {
    if pending.is_empty() {
        return Ok(());
    }
    let batch = std::mem::take(pending);
    let count = batch.len() as u64;
    let last = batch.last().map(|m| (m.env.sess.clone(), m.env.kind.clone()));
    let db = db.clone();
    let now = crate::now_ms();
    let (acks, batch) = tokio::task::spawn_blocking(move || {
        let stored = db.lock().expect("db").store_fresh(&batch, now);
        stored.map(|(acks, fresh)| (acks, fresh.into_iter().map(|i| batch[i].raw.clone()).collect::<Vec<_>>()))
    })
    .await
    .map_err(std::io::Error::other)?
    .map_err(std::io::Error::other)?;
    if let Some(feed) = feed {
        feed.send(now, batch);
    }
    // Only after the commit: acknowledge what is now durable.
    let mut out = Vec::new();
    for (sess, seq) in &acks {
        out.extend(encode_frame(&ack_json(sess, *seq)));
    }
    writer.write_all(&out).await?;
    let mut st = status.lock().expect("status");
    st.frames_received += count;
    st.acks_sent += acks.len() as u64;
    st.last_frame_ms = Some(crate::now_ms());
    if let Some((sess, _)) = last {
        st.last_session = Some(sess);
    }
    Ok(())
}

async fn handle(stream: TcpStream, db: Arc<Mutex<Db>>, status: SharedStatus, feed: Option<Feed>) -> std::io::Result<()> {
    stream.set_nodelay(true)?;
    let (mut reader, mut writer) = stream.into_split();
    let mut decoder = FrameDecoder::new();
    let mut pending: Vec<Incoming> = Vec::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut tick = tokio::time::interval(FLUSH_INTERVAL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            n = reader.read(&mut buf) => {
                let n = n?;
                if n == 0 {
                    break;
                }
                decoder.push(&buf[..n]);
                loop {
                    match decoder.next_frame() {
                        Ok(Some(raw)) => match Envelope::parse(&raw) {
                            Ok(env) => {
                                if env.kind == "init" {
                                    if let Some(g) = serde_json::from_str::<serde_json::Value>(&raw).ok()
                                        .and_then(|v| v.get("game_id").and_then(|g| g.as_str()).map(str::to_owned)) {
                                        status.lock().expect("status").last_game = Some(g);
                                    }
                                }
                                pending.push(Incoming { env, raw });
                            }
                            Err(e) => log!("dropping malformed frame: {e}"),
                        },
                        Ok(None) => break,
                        Err(e) => {
                            flush(&mut pending, &db, &status, &feed, &mut writer).await?;
                            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, e));
                        }
                    }
                }
                if pending.len() >= MAX_BATCH {
                    flush(&mut pending, &db, &status, &feed, &mut writer).await?;
                }
            }
            _ = tick.tick() => {
                flush(&mut pending, &db, &status, &feed, &mut writer).await?;
            }
        }
    }
    // Connection closed: persist what arrived (it will be re-sent by the plugin
    // anyway, because it was never acknowledged).
    let _ = flush(&mut pending, &db, &status, &feed, &mut writer).await;
    Ok(())
}
