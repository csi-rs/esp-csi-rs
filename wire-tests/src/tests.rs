use crate::wire::layout::{Ltf, SPACING_HZ_HE, SPACING_HZ_LEGACY};
use crate::wire::*;
use heapless::Vec;

fn meta() -> RxMeta {
    RxMeta {
        timestamp_us: (1u64 << 32) + 1234, // past the old u32 wrap
        rssi: -42,
        noise_floor: -95,
        channel: 6,
        secondary: Secondary::None,
        bandwidth: Some(Bandwidth::Mhz20),
        ppdu: PpduFormat::HeSu,
        mcs: Some(0),
        stbc: Some(false),
        sgi: None,
        n_rx: 1,
        n_ss: Some(1),
        antenna: None,
        sig_len: 120,
        rx_state: 0,
        not_sounding: Some(true),
        aggregation: None,
        frame_seq: Some(0x123),
        vendor: VendorRx::EspHe {
            rate: 16,
            cur_bb_format: 4,
            estimate_valid: true,
            estimate_len: 490,
            dump_len: 0,
            is_group: false,
            rxend_state: 0,
            rxmatch: 0b0001,
            he_siga1: 0xdead_beef,
            he_siga2: 0x1234,
            sigb_len: 0,
            single_mpdu: false,
        },
    }
}

fn env(seq: u32) -> Envelope {
    Envelope::new([0x24, 0x0a, 0xc4, 1, 2, 3], 0xcafe_f00d, SourceKind::EspVendor, seq)
}

fn round_trip(e: &Envelope, b: &Body) -> (Envelope, Body) {
    let mut buf = [0u8; MAX_ENCODED_LEN];
    let used = encode_cobs(e, b, &mut buf).expect("encode").len();
    assert_eq!(buf[used - 1], 0, "COBS frame ends with its delimiter");
    assert!(!buf[..used - 1].contains(&0), "no delimiter inside the frame");
    decode_cobs(&mut buf[..used]).expect("decode")
}

#[test]
fn esp_raw_round_trips_at_max_size() {
    let mut bytes = Vec::<i8, MAX_CSI_BYTES>::new();
    for i in 0..MAX_CSI_BYTES {
        bytes.push((i as i32 % 256 - 128) as i8).unwrap();
    }
    let frame = CsiFrame::new(
        meta(),
        Stimulus::Ambient { ta: [1, 2, 3, 4, 5, 6] },
        Some(HeaderDigest {
            frame_control: 0x0888,
            addr1: [0xff; 6],
            addr2: [1, 2, 3, 4, 5, 6],
            addr3: [7; 6],
            seq_ctrl: 0x1230,
        }),
        CsiPayload::EspRaw {
            chip: Chip::Esp32C5,
            layout: LayoutId::ClassicBelowHt40Stbc,
            first_word_invalid: true,
            bytes,
        },
    );
    let body = Body::Csi(frame);
    let (e, b) = round_trip(&env(7), &body);
    assert_eq!(e, env(7));
    assert_eq!(b, body);
}

#[test]
fn grouped_and_variation_round_trip() {
    let mut data = Vec::<u8, MAX_CSI_BYTES>::new();
    data.extend_from_slice(&[0xab; 61]).unwrap();
    for payload in [
        CsiPayload::Grouped { ng: 4, nb: 8, sc_start: -122, n_sc: 61, n_rx: 1, n_tx: 2, data },
        CsiPayload::Variation { value: 40_000 },
    ] {
        let body = Body::Csi(CsiFrame::new(
            meta(),
            Stimulus::Controlled { setup_id: 3, instance_id: 65_000, ta: [5; 6] },
            None,
            payload,
        ));
        let e = Envelope::new([9; 6], 1, SourceKind::Synthetic, u32::MAX);
        assert_eq!(round_trip(&e, &body), (e, body));
    }
}

#[test]
fn session_announcement_round_trips() {
    let body = Body::Session(SessionInfo::new(Chip::Esp32S3, [0, 12, 0], 99, Some(1_759_000_000_000_000)));
    assert_eq!(round_trip(&env(0), &body), (env(0), body));
}

#[test]
fn newer_version_is_rejected_before_the_body_is_read() {
    let mut e = env(1);
    e.version = WIRE_VERSION + 1;
    // A body this decoder could never parse: if the version check came after body decoding, this
    // would surface as `Malformed` instead.
    let mut raw = postcard::to_stdvec(&e).unwrap();
    raw.extend_from_slice(&[0xff; 8]);
    let mut framed = vec![0u8; raw.len() + 4];
    let n = cobs::encode(&raw, &mut framed);
    assert_eq!(
        decode_cobs(&mut framed[..n]),
        Err(DecodeError::UnsupportedVersion(WIRE_VERSION + 1))
    );
}

#[test]
fn envelope_is_a_decodable_prefix() {
    let body = Body::Csi(CsiFrame::new(
        meta(),
        Stimulus::Observed { ta: [2; 6], dialog_token: 9 },
        None,
        CsiPayload::Variation { value: 1 },
    ));
    let raw = postcard::to_stdvec(&(env(5), &body)).unwrap();
    let (peeked, _) = postcard::take_from_bytes::<Envelope>(&raw).unwrap();
    assert_eq!(peeked, env(5));
    assert_eq!(decode(&raw).unwrap(), (env(5), body));
}

#[test]
fn garbage_is_malformed_not_a_panic() {
    let mut junk = [0x13u8, 0x37, 0x00];
    assert!(matches!(decode_cobs(&mut junk), Err(DecodeError::Malformed(_))));
    assert!(matches!(decode(&[]), Err(DecodeError::Malformed(_))));
}

/// Totals from ESP-IDF's CSI tables. A transcription error in any range shows up here.
#[test]
fn layout_byte_lengths_match_esp_idf_tables() {
    use LayoutId::*;
    let expect = [
        (ClassicNoneNonHt, 128),
        (ClassicNoneHt20, 256),
        (ClassicNoneHt20Stbc, 384),
        (ClassicBelowNonHt, 128),
        (ClassicBelowHt20, 256),
        (ClassicBelowHt20Stbc, 380),
        (ClassicBelowHt40, 384),
        (ClassicBelowHt40Stbc, 612),
        (ClassicAboveNonHt, 128),
        (ClassicAboveHt20, 256),
        (ClassicAboveHt20Stbc, 376),
        (ClassicAboveHt40, 384),
        (ClassicAboveHt40Stbc, 612),
        (C5LltfNone, 106),
        (C5LltfBelow, 106),
        (C5LltfAbove, 106),
        (C5Ht20None, 114),
        (C5Ht20NoneStbc, 228),
        (C5Ht20Below, 114),
        (C5Ht20BelowStbc, 228),
        (C5Ht20Above, 114),
        (C5Ht20AboveStbc, 228),
        (C5Ht40, 234),
        (C5Ht40Stbc, 468),
        (C5He20Su, 490),
    ];
    for (id, bytes) in expect {
        assert_eq!(id.byte_len(), Some(bytes), "{id:?}");
        assert_eq!(id.indices().count() * 2, bytes, "{id:?}");
    }
    assert_eq!(LayoutId::Unknown.byte_len(), None);
}

#[test]
fn every_layout_fits_the_buffer_and_has_no_duplicate_index_per_field() {
    for id in all_layouts() {
        let len = id.byte_len().unwrap();
        assert!(len <= MAX_CSI_BYTES, "{id:?}");
        for seg in id.segments() {
            let mut seen = std::collections::HashSet::new();
            for i in seg.indices() {
                assert!(seen.insert(i), "{id:?} repeats subcarrier {i} in {:?}", seg.ltf);
            }
        }
    }
}

#[test]
fn classic_classification() {
    // (secondary, sig_mode, forty, stbc, len) -> layout
    assert_eq!(LayoutId::classify_classic(0, 0, false, false, 128), LayoutId::ClassicNoneNonHt);
    assert_eq!(LayoutId::classify_classic(0, 1, false, true, 384), LayoutId::ClassicNoneHt20Stbc);
    assert_eq!(LayoutId::classify_classic(2, 1, true, true, 612), LayoutId::ClassicBelowHt40Stbc);
    assert_eq!(LayoutId::classify_classic(1, 1, false, true, 376), LayoutId::ClassicAboveHt20Stbc);
    // A disabled training field shortens the buffer: refuse to map it.
    assert_eq!(LayoutId::classify_classic(1, 1, true, true, 244), LayoutId::Unknown);
    // VHT does not exist on the classic MAC.
    assert_eq!(LayoutId::classify_classic(0, 3, false, false, 256), LayoutId::Unknown);
}

#[test]
fn c5_classification() {
    assert_eq!(LayoutId::classify_c5(1, 0, 106), LayoutId::C5LltfNone);
    // force_lltf on an HE PPDU still yields the L-LTF layout.
    assert_eq!(LayoutId::classify_c5(4, 2, 106), LayoutId::C5LltfBelow);
    assert_eq!(LayoutId::classify_c5(3, 0, 114), LayoutId::C5Ht20None);
    assert_eq!(LayoutId::classify_c5(2, 1, 468), LayoutId::C5Ht40Stbc);
    assert_eq!(LayoutId::classify_c5(4, 0, 490), LayoutId::C5He20Su);
    // HE MU has no published C5 index map.
    assert_eq!(LayoutId::classify_c5(5, 0, 490), LayoutId::Unknown);
}

#[test]
fn he_layout_spans_the_802_11ax_su_tone_plan() {
    let idx: std::vec::Vec<i16> = LayoutId::C5He20Su.indices().map(|(_, i)| i).collect();
    assert_eq!(idx.len(), 245);
    assert_eq!(*idx.iter().min().unwrap(), -122);
    assert_eq!(*idx.iter().max().unwrap(), 122);
    assert!(LayoutId::C5He20Su.segments().iter().all(|s| s.ltf == Ltf::HeLtf));
    assert_eq!(Ltf::HeLtf.spacing_hz() * 4, SPACING_HZ_LEGACY);
    assert_eq!(SPACING_HZ_HE, 78_125);
}

#[test]
fn header_digest_parsing() {
    // QoS data frame, retry bit set, seq 0x123 frag 0.
    let mut hdr = [0u8; 26];
    hdr[0..2].copy_from_slice(&(0x0888u16 | 1 << 11).to_le_bytes());
    hdr[4..10].copy_from_slice(&[0xaa; 6]);
    hdr[10..16].copy_from_slice(&[0xbb; 6]);
    hdr[16..22].copy_from_slice(&[0xcc; 6]);
    hdr[22..24].copy_from_slice(&(0x123u16 << 4).to_le_bytes());
    let d = HeaderDigest::parse(&hdr).unwrap();
    assert_eq!((d.frame_type(), d.subtype()), (2, 8));
    assert!(d.is_retry());
    assert_eq!(d.addr2, [0xbb; 6]);
    assert_eq!(d.sequence_number(), 0x123);

    // ACK (control, subtype 13): no transmitter address, so no digest.
    let mut ack = [0u8; 24];
    ack[0..2].copy_from_slice(&0x00d4u16.to_le_bytes());
    assert_eq!(HeaderDigest::parse(&ack), None);
    assert_eq!(HeaderDigest::parse(&hdr[..10]), None);
}

#[test]
fn transmitter_prefers_the_header() {
    let mk = |header, stimulus| CsiFrame::new(meta(), stimulus, header, CsiPayload::Variation { value: 0 });
    let h = HeaderDigest { frame_control: 0x0008, addr1: [1; 6], addr2: [2; 6], addr3: [3; 6], seq_ctrl: 0 };
    assert_eq!(mk(Some(h), Stimulus::Ambient { ta: [9; 6] }).transmitter(), Some([2; 6]));
    assert_eq!(mk(None, Stimulus::Ambient { ta: [9; 6] }).transmitter(), Some([9; 6]));
    assert_eq!(mk(None, Stimulus::Controlled { setup_id: 0, instance_id: 0, ta: [4; 6] }).transmitter(), Some([4; 6]));
}

fn all_layouts() -> impl Iterator<Item = LayoutId> {
    use LayoutId::*;
    [
        ClassicNoneNonHt, ClassicNoneHt20, ClassicNoneHt20Stbc, ClassicBelowNonHt, ClassicBelowHt20,
        ClassicBelowHt20Stbc, ClassicBelowHt40, ClassicBelowHt40Stbc, ClassicAboveNonHt,
        ClassicAboveHt20, ClassicAboveHt20Stbc, ClassicAboveHt40, ClassicAboveHt40Stbc, C5LltfNone,
        C5LltfBelow, C5LltfAbove, C5Ht20None, C5Ht20NoneStbc, C5Ht20Below, C5Ht20BelowStbc,
        C5Ht20Above, C5Ht20AboveStbc, C5Ht40, C5Ht40Stbc, C5He20Su,
    ]
    .into_iter()
}

// ── Synthetic 802.11bf source, end to end through the wire format ────────────────────────────

mod synthetic_pipeline {
    use crate::wire::synthetic::SyntheticBf;
    use crate::wire::*;
    use std::collections::BTreeMap;

    const RESPONDERS: [[u8; 6]; 3] = [[0xa0; 6], [0xa1; 6], [0xa2; 6]];

    /// What a host would do: split the byte stream on delimiters, decode each frame, and regroup
    /// reports by measurement instance.
    fn host_ingest(stream: &mut [u8]) -> BTreeMap<(u8, u16), Vec<([u8; 6], u16)>> {
        let mut by_instance: BTreeMap<(u8, u16), Vec<([u8; 6], u16)>> = BTreeMap::new();
        let mut seqs = Vec::new();
        for frame in stream.split_mut(|&b| b == 0).filter(|f| !f.is_empty()) {
            let (env, body) = decode_cobs(frame).expect("every synthetic frame decodes");
            assert_eq!(env.source, SourceKind::Synthetic);
            seqs.push(env.stream_seq);
            let Body::Csi(f) = body else { panic!("only measurement frames expected") };
            let Stimulus::Controlled { setup_id, instance_id, ta } = f.stimulus else {
                panic!("an 11bf report is a controlled stimulus")
            };
            let CsiPayload::Grouped { ng, nb, sc_start, n_sc, data, .. } = f.payload else {
                panic!("an 11bf report is grouped")
            };
            assert_eq!((ng, nb, sc_start), (4, 8, -122));
            assert_eq!(data.len(), n_sc as usize);
            by_instance.entry((setup_id, instance_id)).or_default().push((ta, n_sc));
        }
        // The envelope counter is contiguous: nothing was lost between "device" and host.
        assert!(seqs.windows(2).all(|w| w[1] == w[0] + 1));
        by_instance
    }

    #[test]
    fn every_instance_reaches_the_host_with_every_responder() {
        let mut src = SyntheticBf::new(7, RESPONDERS, 4, 10_000, 42);
        let mut stream = Vec::new();
        let mut buf = [0u8; MAX_ENCODED_LEN];
        for seq in 0..(3 * 50) {
            let env = Envelope::new([1; 6], 0xfeed, SourceKind::Synthetic, seq);
            let n = encode_cobs(&env, &Body::Csi(src.next_frame()), &mut buf).unwrap().len();
            stream.extend_from_slice(&buf[..n]);
        }
        let instances = host_ingest(&mut stream);
        assert_eq!(instances.len(), 50);
        for ((setup, _), reports) in &instances {
            assert_eq!(*setup, 7);
            let mut tas: Vec<_> = reports.iter().map(|(ta, _)| *ta).collect();
            tas.sort();
            assert_eq!(tas, RESPONDERS.to_vec());
            assert!(reports.iter().all(|(_, n_sc)| *n_sc == 62)); // ceil(245 / 4)
        }
    }

    #[test]
    fn deterministic_for_a_seed() {
        let mut a = SyntheticBf::new(1, RESPONDERS, 2, 1000, 99);
        let mut b = SyntheticBf::new(1, RESPONDERS, 2, 1000, 99);
        for _ in 0..30 {
            assert_eq!(a.next_frame(), b.next_frame());
        }
    }

    #[test]
    fn timestamps_advance_by_the_periodicity() {
        let mut src = SyntheticBf::new(1, [[1; 6]], 1, 25_000, 1);
        let t0 = src.next_frame().meta.timestamp_us;
        let t1 = src.next_frame().meta.timestamp_us;
        assert_eq!(t1 - t0, 25_000);
    }
}

// ── Reporting policy ─────────────────────────────────────────────────────────────────────────

mod policy {
    use crate::policy::{Decimator, ThresholdState};

    fn buf(level: i8, len: usize) -> Vec<i8> {
        (0..len).map(|i| if i % 2 == 0 { level } else { level / 2 }).collect()
    }

    #[test]
    fn steady_channel_scores_zero_and_a_shape_change_scores_high() {
        let mut t = ThresholdState::new();
        assert_eq!(t.score(&buf(40, 128)), 0, "first buffer primes the average");
        for _ in 0..50 {
            assert_eq!(t.score(&buf(40, 128)), 0);
        }
        // Half the subcarriers doubled: the profile's shape changed.
        let mut moved = buf(40, 128);
        for v in moved.iter_mut().take(64) {
            *v = v.saturating_mul(2);
        }
        let s = t.score(&moved);
        assert!(s > 5_000, "a shape change scores well above zero: {s}");
    }

    #[test]
    fn a_gain_change_alone_scores_nothing() {
        let mut t = ThresholdState::new();
        let shaped: Vec<i8> = (0..128).map(|i| (10 + (i % 23)) as i8).collect();
        t.score(&shaped);
        for _ in 0..20 {
            t.score(&shaped);
        }
        let louder: Vec<i8> = shaped.iter().map(|v| v.saturating_mul(3)).collect();
        let s = t.score(&louder);
        assert!(s < 300, "uniform scaling is the receiver's gain, not the channel: {s}");
    }

    #[test]
    fn a_new_layout_restarts_the_average() {
        let mut t = ThresholdState::new();
        t.score(&buf(40, 128));
        assert_eq!(t.score(&buf(90, 234)), 0);
    }

    #[test]
    fn hold_window_keeps_reporting_after_a_crossing() {
        let mut t = ThresholdState::new();
        assert!(t.decide(5000, 2000, 100, 1_000_000));
        assert!(t.decide(0, 2000, 100, 1_050_000), "inside the 100 ms hold");
        assert!(!t.decide(0, 2000, 100, 1_100_001), "hold expired");
    }

    #[test]
    fn decimation_keeps_one_in_n() {
        let mut d = Decimator::new();
        let kept: Vec<bool> = (0..9).map(|_| d.tick(3)).collect();
        assert_eq!(kept, [true, false, false, true, false, false, true, false, false]);
        let mut d = Decimator::new();
        assert!((0..5).all(|_| d.tick(0)), "0 is treated as 1");
    }
}

// ── Measurement setup ────────────────────────────────────────────────────────────────────────

mod setup {
    use crate::wire::*;

    #[test]
    fn setup_round_trips_through_postcard() {
        let mut st = StimulusParams::new(5_000, Bandwidth::Mhz20);
        st.responders.push([3; 6]).unwrap();
        let setup = MeasurementSetup::new(9)
            .with_acquisition(AcquisitionFilter {
                ppdu_formats: PpduSet::ANY.with(PpduFormat::HeSu).with(PpduFormat::HeMu),
                ..AcquisitionFilter::default()
            })
            .with_stimulus(st);
        let bytes = postcard::to_stdvec(&setup).unwrap();
        assert_eq!(postcard::from_bytes::<MeasurementSetup>(&bytes).unwrap(), setup);
        assert_eq!(setup.stimulus.unwrap().rate_hz(), 200);
    }

    #[test]
    fn ppdu_set_queries() {
        assert!(PpduSet::ANY.contains(PpduFormat::Dsss));
        let he = PpduSet::ANY.with(PpduFormat::HeSu);
        assert!(he.only_he() && he.excludes_legacy() && !he.contains(PpduFormat::Ht));
        let ht = PpduSet::ANY.with(PpduFormat::Ht).with(PpduFormat::Vht);
        assert!(!ht.only_he() && ht.excludes_legacy());
        assert!(!PpduSet::ANY.with(PpduFormat::NonHt).excludes_legacy());
    }

    #[test]
    fn rate_saturates() {
        assert_eq!(StimulusParams::new(0, Bandwidth::Mhz20).rate_hz(), u16::MAX);
        assert_eq!(StimulusParams::new(10, Bandwidth::Mhz20).rate_hz(), u16::MAX);
        assert_eq!(StimulusParams::new(5_000_000, Bandwidth::Mhz20).rate_hz(), 1);
    }
}
