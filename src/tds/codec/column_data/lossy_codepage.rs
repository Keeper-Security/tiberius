use super::*;
use crate::sql_read_bytes::test_utils::IntoSqlReadBytes;
use crate::tds::Collation;
use crate::VarLenContext;
use bytes::{BufMut, BytesMut};

fn type_info(ty: VarLenType, sort_id: u8) -> TypeInfo {
    TypeInfo::VarLenSized(VarLenContext::new(
        ty,
        64,
        Some(Collation::new(0x804, sort_id)),
    ))
}

fn frame(ty: VarLenType, payload: &[u8]) -> BytesMut {
    let mut wire = BytesMut::new();
    if matches!(ty, VarLenType::Text | VarLenType::NText) {
        wire.put_u8(1);
        wire.put_u8(0);
        wire.put_u64_le(0);
        wire.put_u32_le(payload.len() as u32);
    } else {
        wire.put_u16_le(payload.len() as u16);
    }
    wire.extend_from_slice(payload);
    wire
}

async fn decode(
    type_info: &TypeInfo,
    wire: BytesMut,
    lossy: bool,
) -> crate::Result<ColumnData<'static>> {
    let mut reader = wire.into_sql_read_bytes();
    reader.context_mut().set_lossy_codepage(lossy);
    ColumnData::decode(&mut reader, type_info).await
}

#[tokio::test]
async fn codepage_lossy_keeps_following_values_readable() {
    for ty in [
        VarLenType::BigChar,
        VarLenType::BigVarChar,
        VarLenType::Text,
    ] {
        let mut wire = frame(ty, b"bad\x81 ");
        wire.extend_from_slice(&frame(ty, b"next"));
        let mut reader = wire.into_sql_read_bytes();
        reader.context_mut().set_lossy_codepage(true);
        let first = ColumnData::decode(&mut reader, &type_info(ty, 198))
            .await
            .unwrap();
        assert_eq!(first, ColumnData::String(Some("bad\u{fffd} ".into())));
        let second = ColumnData::decode(&mut reader, &type_info(ty, 198))
            .await
            .unwrap();
        assert_eq!(second, ColumnData::String(Some("next".into())));
    }
}

#[test]
fn codepage_lossy_context_is_independent_of_utf16() {
    let mut reader = BytesMut::new().into_sql_read_bytes();
    assert!(!reader.context().lossy_codepage());
    reader.context_mut().set_lossy_codepage(true);
    assert!(!reader.context().lossy_utf16());
    reader.context_mut().set_lossy_utf16(true);
    reader.context_mut().set_lossy_codepage(false);
    assert!(!reader.context().lossy_codepage());
    assert!(reader.context().lossy_utf16());
}

#[tokio::test]
async fn codepage_lossy_preserves_legacy_codepages() {
    for (sort_id, payload, expected) in [
        (30, b"Caf\x82 \xe0\xff".as_slice(), "Café α\u{a0}"),
        (49, b"Caf\x82 \x9b\xff".as_slice(), "Café ø\u{a0}"),
    ] {
        let collation = Collation::new(0x409, sort_id);
        for lossy in [false, true] {
            for ty in [
                VarLenType::BigChar,
                VarLenType::BigVarChar,
                VarLenType::Text,
            ] {
                let info = TypeInfo::VarLenSized(VarLenContext::new(ty, 64, Some(collation)));
                let value = decode(&info, frame(ty, payload), lossy).await.unwrap();
                assert_eq!(value, ColumnData::String(Some(expected.into())));
            }

            let mut wire = BytesMut::new();
            wire.put_u64_le(payload.len() as u64);
            wire.put_u32_le(3);
            wire.extend_from_slice(&payload[..3]);
            wire.put_u32_le((payload.len() - 3) as u32);
            wire.extend_from_slice(&payload[3..]);
            wire.put_u32_le(0);
            let info = TypeInfo::VarLenSized(VarLenContext::new(
                VarLenType::BigVarChar,
                0xffff,
                Some(collation),
            ));
            let value = decode(&info, wire, lossy).await.unwrap();
            assert_eq!(value, ColumnData::String(Some(expected.into())));

            for ty in [VarLenType::BigChar, VarLenType::BigVarChar] {
                let mut wire = BytesMut::new();
                wire.put_u32_le((9 + payload.len()) as u32);
                wire.put_u8(ty as u8);
                wire.put_u8(7);
                wire.put_u32_le(collation.info());
                wire.put_u8(sort_id);
                wire.put_u16_le(payload.len() as u16);
                wire.extend_from_slice(payload);
                let info =
                    TypeInfo::VarLenSized(VarLenContext::new(VarLenType::SSVariant, 8016, None));
                let value = decode(&info, wire, lossy).await.unwrap();
                assert_eq!(value, ColumnData::String(Some(expected.into())));
            }
        }
    }
}

#[test]
fn codepage_lossy_does_not_relax_outgoing_encoding() {
    for ty in [
        VarLenType::BigChar,
        VarLenType::BigVarChar,
        VarLenType::Text,
    ] {
        let mut encoded = BytesMut::new();
        let type_info = type_info(ty, 192);
        let error = ColumnData::String(Some("🙂".into()))
            .encode(&mut BytesMutWithTypeInfo::new(&mut encoded).with_type_info(&type_info))
            .unwrap_err();
        assert!(matches!(error, crate::Error::Encoding(_)));
    }
}

#[tokio::test]
async fn codepage_lossy_replaces_invalid_row_bytes() {
    for sort_id in [192, 194, 198] {
        for ty in [
            VarLenType::BigChar,
            VarLenType::BigVarChar,
            VarLenType::Text,
        ] {
            let wire = frame(ty, b"ok\x81 ok");
            let value = decode(&type_info(ty, sort_id), wire.clone(), true)
                .await
                .unwrap();
            assert_eq!(value, ColumnData::String(Some("ok\u{fffd} ok".into())));
            let error = decode(&type_info(ty, sort_id), wire, false)
                .await
                .unwrap_err();
            assert!(matches!(error, crate::Error::Encoding(_)));
        }
    }
}

#[tokio::test]
async fn codepage_lossy_preserves_bom_like_collation_bytes() {
    for lossy in [false, true] {
        for ty in [
            VarLenType::BigChar,
            VarLenType::BigVarChar,
            VarLenType::Text,
        ] {
            let value = decode(&type_info(ty, 198), frame(ty, b"\xef\xbb\xbfabc"), lossy)
                .await
                .unwrap();
            assert_eq!(value, ColumnData::String(Some("锘縜bc".into())));
        }
    }
}

#[tokio::test]
async fn codepage_lossy_preserves_empty_and_null_values() {
    for lossy in [false, true] {
        for ty in [
            VarLenType::BigChar,
            VarLenType::BigVarChar,
            VarLenType::Text,
        ] {
            let value = decode(&type_info(ty, 198), frame(ty, b""), lossy)
                .await
                .unwrap();
            assert_eq!(value, ColumnData::String(Some("".into())));
            let null: &[u8] = if ty == VarLenType::Text {
                &[0]
            } else {
                &[255, 255]
            };
            let value = decode(&type_info(ty, 198), BytesMut::from(null), lossy)
                .await
                .unwrap();
            assert_eq!(value, ColumnData::String(None));
        }
    }
}

#[tokio::test]
async fn codepage_lossy_retains_encoding_and_framing_errors() {
    for ty in [
        VarLenType::BigChar,
        VarLenType::BigVarChar,
        VarLenType::Text,
    ] {
        let error = decode(&type_info(ty, 255), frame(ty, b"valid"), true)
            .await
            .unwrap_err();
        assert!(matches!(error, crate::Error::Encoding(_)));
        let mut truncated = frame(ty, b"valid");
        truncated.truncate(truncated.len() - 1);
        assert!(decode(&type_info(ty, 198), truncated, true).await.is_err());
    }
}

#[tokio::test]
async fn codepage_lossy_does_not_relax_unicode_or_xml() {
    for ty in [VarLenType::NChar, VarLenType::NVarchar, VarLenType::NText] {
        assert!(decode(&type_info(ty, 52), frame(ty, &[0, 0xd8]), true)
            .await
            .is_err());
        assert!(decode(&type_info(ty, 52), frame(ty, &[0]), true)
            .await
            .is_err());
    }
    let xml_type = TypeInfo::Xml {
        schema: None,
        size: 64,
    };
    assert!(
        decode(&xml_type, frame(VarLenType::NVarchar, &[0, 0xd8]), true)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn codepage_lossy_handles_chunked_varchar_max() {
    let type_info = TypeInfo::VarLenSized(VarLenContext::new(
        VarLenType::BigVarChar,
        0xffff,
        Some(Collation::new(0x804, 198)),
    ));
    let mut wire = BytesMut::new();
    wire.put_u64_le(5);
    wire.put_u32_le(3);
    wire.extend_from_slice(b"ok\x81");
    wire.put_u32_le(2);
    wire.extend_from_slice(b" !");
    wire.put_u32_le(0);
    let value = decode(&type_info, wire.clone(), true).await.unwrap();
    assert_eq!(value, ColumnData::String(Some("ok\u{fffd} !".into())));
    assert!(decode(&type_info, wire, false).await.is_err());
}

#[tokio::test]
async fn codepage_lossy_handles_sql_variant_character_values() {
    for ty in [VarLenType::BigChar, VarLenType::BigVarChar] {
        let mut wire = BytesMut::new();
        wire.put_u32_le(16);
        wire.put_u8(ty as u8);
        wire.put_u8(7);
        wire.put_u32_le(0x804);
        wire.put_u8(198);
        wire.put_u16_le(7);
        wire.extend_from_slice(b"ok\x81 end");
        let type_info =
            TypeInfo::VarLenSized(VarLenContext::new(VarLenType::SSVariant, 8016, None));
        let value = decode(&type_info, wire.clone(), true).await.unwrap();
        assert_eq!(value, ColumnData::String(Some("ok\u{fffd} end".into())));
        assert!(decode(&type_info, wire, false).await.is_err());
    }
}
