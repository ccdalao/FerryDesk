use ferry_proto::limits::MAX_CHUNK_LEN;
use ferry_proto::{DecodeError, DisplayAssembler, DisplayPacket, DisplayTrack, MAX_ACCESS_UNIT};

fn fragment(track: DisplayTrack, timestamp_us: u64, end: bool, bytes: Vec<u8>) -> DisplayPacket {
    DisplayPacket {
        track,
        timestamp_us,
        end,
        bytes,
    }
}

#[test]
fn reconstructs_one_encoded_unit_from_ordered_fragments() {
    let mut assembler = DisplayAssembler::default();
    assert_eq!(
        assembler
            .push(fragment(DisplayTrack::Video, 10, false, vec![1, 2]))
            .unwrap(),
        None
    );
    let frame = assembler
        .push(fragment(DisplayTrack::Video, 10, true, vec![3]))
        .unwrap()
        .unwrap();
    assert_eq!(
        (frame.track, frame.timestamp_us, frame.bytes),
        (DisplayTrack::Video, 10, vec![1, 2, 3])
    );
    assert!(
        assembler
            .push(fragment(DisplayTrack::Audio, 12, true, vec![4]))
            .unwrap()
            .is_some()
    );
}

#[test]
fn refuses_stalled_or_mixed_fragments() {
    let mut assembler = DisplayAssembler::default();
    for _ in 0..MAX_ACCESS_UNIT / MAX_CHUNK_LEN {
        assert_eq!(
            assembler
                .push(fragment(
                    DisplayTrack::Video,
                    10,
                    false,
                    vec![0; MAX_CHUNK_LEN]
                ))
                .unwrap(),
            None
        );
    }
    assert!(matches!(
        assembler.push(fragment(DisplayTrack::Audio, 10, true, vec![4])),
        Err(DecodeError::InvalidValue(_))
    ));
    assert!(matches!(
        assembler.push(fragment(DisplayTrack::Video, 11, true, vec![4])),
        Err(DecodeError::InvalidValue(_))
    ));
    assert!(matches!(
        assembler.push(fragment(DisplayTrack::Video, 10, true, vec![4])),
        Err(DecodeError::LimitExceeded(_))
    ));
}
