//! The scorer with a lot of data (2026-10-15, the owner's capture-mode World of
//! Trucks job): small live chunks are worked through in bounded passes, and a
//! job whose data could not be written is written on the next pass.

mod common;

use common::seed::*;
use common::TestHost;
use pt_server::scorer::{Scorer, MAX_CHUNKS_PER_PASS};

#[test]
fn many_small_chunks_take_several_passes() {
    let t = TestHost::new();
    publish(&t);
    let token = link(&t);
    // 20 Hz for 3 minutes, one chunk per 4 frames: like a live upload.
    let lines = synthetic_job("0123456789abcdef0123456789abcdef", 50.0, 180.0);
    upload(&t, &token, &lines, 4);
    let chunks = lines.len().div_ceil(4);
    assert!(chunks > 2 * MAX_CHUNKS_PER_PASS, "{chunks}");

    let mut s = Scorer::new(&user_id(&t), pt_core::Game::Ats);
    let mut passes = Vec::new();
    loop {
        let r = common::block_on(s.process(&t)).unwrap();
        assert!(r.chunks <= MAX_CHUNKS_PER_PASS);
        passes.push(r.chunks);
        if !r.more {
            break;
        }
    }
    assert_eq!(passes.iter().sum::<usize>(), chunks, "{passes:?}");
    assert_eq!(passes.len(), chunks.div_ceil(MAX_CHUNKS_PER_PASS));
    let job = t.q("SELECT job_id, state FROM jobs");
    assert_eq!(job.len(), 1);
    assert_eq!(job[0]["state"], "delivered");
    // Its data is there, and the series reads it.
    let id = job[0]["job_id"].as_str().unwrap();
    let r = t.call(t.admin("GET", &format!("/api/v1/admin/jobs/{id}/series?max_points=50")));
    assert_eq!(r.status, 200, "{}", r.body_json());
}

#[test]
fn a_rebuild_in_several_passes_never_stores_a_job_half_replayed() {
    // 2026-10-15: a refresh replayed the owner's capture-mode session, and the
    // first pass wrote the delivered job back to `tracking` for a few minutes.
    let t = TestHost::new();
    publish(&t);
    let token = link(&t);
    let lines = synthetic_job("0123456789abcdef0123456789abcdef", 50.0, 180.0);
    upload(&t, &token, &lines, 4);
    let mut s = Scorer::new(&user_id(&t), pt_core::Game::Ats);
    while common::block_on(s.process(&t)).unwrap().more {}
    let before = t.q("SELECT job_id, state, outcome, version FROM jobs");
    assert_eq!(before[0]["state"], "delivered");
    let events = t.q("SELECT event_id FROM webhook_outbox").len();

    common::block_on(s.reset(&t)).unwrap();
    let mut passes = 0;
    loop {
        let r = common::block_on(s.process(&t)).unwrap();
        passes += 1;
        let now = t.q("SELECT job_id, state, outcome, version FROM jobs");
        assert_eq!(now[0]["state"], "delivered", "pass {passes}");
        assert_eq!(now[0]["outcome"], before[0]["outcome"], "pass {passes}");
        if !r.more {
            break;
        }
        assert_eq!(r.jobs_stored, 0, "pass {passes}");
    }
    assert!(passes > 2, "{passes}");
    assert_eq!(t.q("SELECT job_id FROM jobs").len(), 1);
    // Nothing changed, so nothing was announced.
    assert_eq!(t.q("SELECT event_id FROM webhook_outbox").len(), events);
}

#[test]
fn a_job_whose_data_failed_to_store_is_stored_on_the_next_pass() {
    let t = TestHost::new();
    publish(&t);
    let token = link(&t);
    upload(&t, &token, &synthetic_job("0123456789abcdef0123456789abcdef", 50.0, 60.0), 200);
    let mut s = Scorer::new(&user_id(&t), pt_core::Game::Ats);
    t.fail_job_puts.set(1);
    assert!(common::block_on(s.process(&t)).is_err(), "the write failed");
    assert!(t.blobs.borrow().keys().all(|k| !k.starts_with("jobs/")));
    assert!(t.q("SELECT job_id FROM jobs").is_empty(), "no row without its data");

    // Nothing new uploaded: the next pass still stores it.
    let r = common::block_on(s.process(&t)).unwrap();
    assert_eq!(r.jobs_stored, 1);
    let id = t.q("SELECT job_id FROM jobs")[0]["job_id"].as_str().unwrap().to_string();
    assert!(t.blobs.borrow().contains_key(&format!("jobs/{id}.json.gz")));
    assert_eq!(t.call(t.admin("GET", &format!("/api/v1/admin/jobs/{id}/series"))).status, 200);
}

#[test]
fn a_row_whose_data_went_missing_gets_it_back() {
    let t = TestHost::new();
    publish(&t);
    let token = link(&t);
    upload(&t, &token, &synthetic_job("0123456789abcdef0123456789abcdef", 50.0, 60.0), 200);
    score(&t, pt_core::Game::Ats);
    let id = t.q("SELECT job_id FROM jobs")[0]["job_id"].as_str().unwrap().to_string();
    // As production had it: the row, without its data.
    t.blobs.borrow_mut().remove(&format!("jobs/{id}.json.gz"));
    assert_eq!(t.call(t.admin("GET", &format!("/api/v1/admin/jobs/{id}/series"))).status, 404);
    // Replaying the raw data (a new scorer: no snapshot in memory) restores it.
    t.blobs.borrow_mut().retain(|k, _| !k.starts_with("snap/"));
    score(&t, pt_core::Game::Ats);
    assert_eq!(t.call(t.admin("GET", &format!("/api/v1/admin/jobs/{id}/series"))).status, 200);
}
