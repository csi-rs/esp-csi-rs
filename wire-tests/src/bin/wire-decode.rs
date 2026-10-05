//! Decode a raw serial capture of `LogMode::Serialized` output and summarise it.
//!
//! `wire-decode <capture.bin> [--text]` — frames are split on the COBS delimiter; each is decoded
//! with the crate's own `wire::decode_cobs`. Frames that do not decode but are printable are the
//! device's framed log lines; `--text` prints them.

use std::collections::BTreeMap;

use esp_csi_rs_wire_tests::wire::*;

fn printable(f: &[u8]) -> bool {
    !f.is_empty() && f.iter().all(|&b| b == b'\r' || b == b'\n' || b == b'\t' || (0x20..0x7f).contains(&b))
}

#[derive(Default)]
struct Summary {
    csi: u64,
    sessions: Vec<(Envelope, SessionInfo)>,
    decode_errors: u64,
    version_errors: u64,
    text: Vec<String>,
    seq_gaps: u64,
    seq_lost: u64,
    seq_resets: u64,
    ts_backwards: u64,
    ts_first: Option<u64>,
    ts_last: u64,
    ts_max: u64,
    sources: BTreeMap<String, u64>,
    ppdu: BTreeMap<String, u64>,
    layout: BTreeMap<String, u64>,
    lens: BTreeMap<usize, u64>,
    payload: BTreeMap<&'static str, u64>,
    stimulus: BTreeMap<&'static str, u64>,
    tx: BTreeMap<[u8; 6], u64>,
    setup_ids: BTreeMap<u8, u64>,
    instances: std::collections::BTreeSet<u16>,
    header: u64,
    retries: u64,
    node_ids: BTreeMap<[u8; 6], u64>,
    session_ids: BTreeMap<u32, u64>,
    variation: Vec<u16>,
    bandwidth: BTreeMap<String, u64>,
}

fn mac(m: &[u8; 6]) -> String {
    m.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(":")
}

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("usage: wire-decode <capture.bin> [--text]");
    let show_text = args.any(|a| a == "--text");
    let mut data = std::fs::read(&path).expect("read capture");
    let mut s = Summary::default();
    let mut last_seq: Option<(u32, u32)> = None; // (session, seq)

    for frame in data.split_mut(|&b| b == 0) {
        if frame.is_empty() {
            continue;
        }
        let raw = frame.to_vec();
        match decode_cobs(frame) {
            Ok((env, body)) => {
                *s.node_ids.entry(env.node_id).or_default() += 1;
                *s.session_ids.entry(env.session_id).or_default() += 1;
                match body {
                    Body::Session(info) => s.sessions.push((env, info)),
                    Body::Csi(f) => {
                        s.csi += 1;
                        *s.sources.entry(format!("{:?}", env.source)).or_default() += 1;
                        if let Some((sess, seq)) = last_seq {
                            if sess != env.session_id || env.stream_seq < seq {
                                s.seq_resets += 1;
                            } else if env.stream_seq > seq + 1 {
                                s.seq_gaps += 1;
                                s.seq_lost += (env.stream_seq - seq - 1) as u64;
                            }
                        }
                        last_seq = Some((env.session_id, env.stream_seq));
                        let ts = f.meta.timestamp_us;
                        if s.ts_first.is_none() {
                            s.ts_first = Some(ts);
                        } else if ts < s.ts_last {
                            s.ts_backwards += 1;
                        }
                        s.ts_last = ts;
                        s.ts_max = s.ts_max.max(ts);
                        *s.ppdu.entry(format!("{:?}", f.meta.ppdu)).or_default() += 1;
                        *s.bandwidth.entry(format!("{:?}", f.meta.bandwidth)).or_default() += 1;
                        if let Some(t) = f.transmitter() {
                            *s.tx.entry(t).or_default() += 1;
                        }
                        if let Some(h) = f.header {
                            s.header += 1;
                            if h.is_retry() {
                                s.retries += 1;
                            }
                        }
                        let st = match f.stimulus {
                            Stimulus::Controlled { setup_id, instance_id, .. } => {
                                *s.setup_ids.entry(setup_id).or_default() += 1;
                                s.instances.insert(instance_id);
                                "controlled"
                            }
                            Stimulus::Ambient { .. } => "ambient",
                            Stimulus::Observed { .. } => "observed",
                            _ => "other",
                        };
                        *s.stimulus.entry(st).or_default() += 1;
                        let kind = match &f.payload {
                            CsiPayload::EspRaw { layout, bytes, .. } => {
                                *s.layout.entry(format!("{layout:?}")).or_default() += 1;
                                *s.lens.entry(bytes.len()).or_default() += 1;
                                "esp-raw"
                            }
                            CsiPayload::Grouped { .. } => "grouped",
                            CsiPayload::Variation { value } => {
                                s.variation.push(*value);
                                "variation"
                            }
                            _ => "other",
                        };
                        *s.payload.entry(kind).or_default() += 1;
                    }
                    _ => {}
                }
            }
            Err(DecodeError::UnsupportedVersion(_)) => s.version_errors += 1,
            Err(_) if printable(&raw) => s.text.push(String::from_utf8_lossy(&raw).trim().to_string()),
            Err(_) => s.decode_errors += 1,
        }
    }

    println!("file: {path}");
    println!("csi frames: {}   decode errors: {}   version errors: {}   text lines: {}", s.csi, s.decode_errors, s.version_errors, s.text.len());
    for (env, info) in &s.sessions {
        println!(
            "session: id={:#010x} node={} chip={:?} crate={:?} anchor_ts={} epoch={:?} seq={}",
            env.session_id, mac(&env.node_id), info.chip, info.crate_version, info.timestamp_us, info.epoch_unix_us, env.stream_seq
        );
    }
    println!("node ids: {:?}", s.node_ids.iter().map(|(k, v)| (mac(k), *v)).collect::<Vec<_>>());
    println!("session ids: {:?}", s.session_ids.iter().map(|(k, v)| (format!("{k:#010x}"), *v)).collect::<Vec<_>>());
    println!("stream_seq: gaps={} lost={} resets={}", s.seq_gaps, s.seq_lost, s.seq_resets);
    if let Some(first) = s.ts_first {
        let span = s.ts_last.saturating_sub(first);
        println!(
            "timestamp_us: first={} last={} max={} span={:.1}s backwards={} rate={:.1}/s exceeds_u32={}",
            first, s.ts_last, s.ts_max, span as f64 / 1e6, s.ts_backwards,
            if span > 0 { s.csi as f64 / (span as f64 / 1e6) } else { 0.0 },
            s.ts_max > u32::MAX as u64
        );
    }
    println!("sources: {:?}", s.sources);
    println!("ppdu: {:?}", s.ppdu);
    println!("bandwidth: {:?}", s.bandwidth);
    println!("payload: {:?}", s.payload);
    println!("layout: {:?}", s.layout);
    println!("buffer lengths: {:?}", s.lens);
    println!("stimulus: {:?}", s.stimulus);
    if !s.setup_ids.is_empty() {
        println!(
            "setup ids: {:?}  instances: {} distinct, {:?}..{:?}",
            s.setup_ids, s.instances.len(), s.instances.first(), s.instances.last()
        );
    }
    println!("header digest: {} frames, {} retries", s.header, s.retries);
    let mut tx: Vec<_> = s.tx.iter().collect();
    tx.sort_by(|a, b| b.1.cmp(a.1));
    println!("top transmitters: {:?}", tx.iter().take(6).map(|(m, c)| (mac(m), **c)).collect::<Vec<_>>());
    if !s.variation.is_empty() {
        let mx = s.variation.iter().max().unwrap();
        let mn = s.variation.iter().min().unwrap();
        let mut v = s.variation.clone();
        v.sort();
        let pct = |p: usize| v[(v.len() - 1) * p / 100];
        println!(
            "variation values: n={} min={} p50={} p90={} p99={} max={}",
            v.len(), mn, pct(50), pct(90), pct(99), mx
        );
    }
    let stats: Vec<_> = s.text.iter().filter(|t| t.starts_with("STATS") || t.starts_with("TX ") || t.starts_with("HWCHECK")).collect();
    for t in stats.iter().rev().take(8).rev() {
        println!("  | {t}");
    }
    if show_text {
        for t in &s.text {
            println!("text: {t}");
        }
    }
}
