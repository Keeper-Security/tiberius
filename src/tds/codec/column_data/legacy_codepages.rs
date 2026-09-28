use super::*;
use crate::sql_read_bytes::test_utils::IntoSqlReadBytes;
use crate::tds::Collation;
use crate::VarLenContext;
use bytes::{BufMut, BytesMut};

const CP437_EXPECTED_HIGH: &str = concat!(
    "ÇüéâäàåçêëèïîìÄÅ",
    "ÉæÆôöòûùÿÖÜ¢£¥₧ƒ",
    "áíóúñÑªº¿⌐¬½¼¡«»",
    "░▒▓│┤╡╢╖╕╣║╗╝╜╛┐",
    "└┴┬├─┼╞╟╚╔╩╦╠═╬╧",
    "╨╤╥╙╘╒╓╫╪┘┌█▄▌▐▀",
    "αßΓπΣσµτΦΘΩδ∞φε∩",
    "≡±≥≤⌠⌡÷≈°∙·√ⁿ²■\u{a0}",
);

const CP850_EXPECTED_HIGH: &str = concat!(
    "ÇüéâäàåçêëèïîìÄÅ",
    "ÉæÆôöòûùÿÖÜø£Ø×ƒ",
    "áíóúñÑªº¿®¬½¼¡«»",
    "░▒▓│┤ÁÂÀ©╣║╗╝¢¥┐",
    "└┴┬├─┼ãÃ╚╔╩╦╠═╬¤",
    "ðÐÊËÈıÍÎÏ┘┌█▄¦Ì▀",
    "ÓßÔÒõÕµþÞÚÛÙýÝ¯´",
    "\u{ad}±‗¾¶§÷¸°¨·¹³²■\u{a0}",
);

fn wire_value(ty: VarLenType, payload: &[u8]) -> BytesMut {
    let mut wire = BytesMut::new();
    if ty == VarLenType::Text {
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

#[tokio::test]
async fn legacy_codepages_decode_raw_char_varchar_and_text() {
    for (sort_id, payload, expected) in [
        (30, b"Caf\x82 \xe0\xff".as_slice(), "Café α\u{a0}"),
        (49, b"Caf\x82 \x9b\xff".as_slice(), "Café ø\u{a0}"),
    ] {
        for ty in [
            VarLenType::BigChar,
            VarLenType::BigVarChar,
            VarLenType::Text,
        ] {
            let type_info = TypeInfo::VarLenSized(VarLenContext::new(
                ty,
                32,
                Some(Collation::new(0x409, sort_id)),
            ));
            let mut reader = wire_value(ty, payload).into_sql_read_bytes();
            let decoded = ColumnData::decode(&mut reader, &type_info).await.unwrap();
            assert_eq!(decoded, ColumnData::String(Some(expected.into())));
        }
    }
}

#[tokio::test]
async fn legacy_codepages_match_unicode_mappings_for_every_byte() {
    let payload: Vec<u8> = (0..=255).collect();
    let ascii: String = (0..=127).map(char::from).collect();
    for sort_id in (30..=35).chain(40..=45).chain([49]).chain(55..=61) {
        let high = if sort_id <= 35 {
            CP437_EXPECTED_HIGH
        } else {
            CP850_EXPECTED_HIGH
        };
        let expected = format!("{ascii}{high}");
        for ty in [
            VarLenType::BigChar,
            VarLenType::BigVarChar,
            VarLenType::Text,
        ] {
            let type_info = TypeInfo::VarLenSized(VarLenContext::new(
                ty,
                256,
                Some(Collation::new(0x409, sort_id)),
            ));
            let mut reader = wire_value(ty, &payload).into_sql_read_bytes();
            let decoded = ColumnData::decode(&mut reader, &type_info).await.unwrap();
            assert_eq!(
                decoded,
                ColumnData::String(Some(expected.as_str().into())),
                "sort ID {sort_id}, type {ty:?}",
            );
            let mut encoded = BytesMut::new();
            decoded
                .encode(&mut BytesMutWithTypeInfo::new(&mut encoded).with_type_info(&type_info))
                .unwrap();
            assert!(encoded.ends_with(&payload));
            let mut reader = encoded.into_sql_read_bytes();
            let decoded = ColumnData::decode(&mut reader, &type_info).await.unwrap();
            assert_eq!(decoded, ColumnData::String(Some(expected.as_str().into())));
        }
    }
}

#[test]
fn legacy_codepages_reject_unrepresentable_bulk_values() {
    for sort_id in [30, 49] {
        for ty in [
            VarLenType::BigChar,
            VarLenType::BigVarChar,
            VarLenType::Text,
        ] {
            let type_info = TypeInfo::VarLenSized(VarLenContext::new(
                ty,
                32,
                Some(Collation::new(0x409, sort_id)),
            ));
            let mut encoded = BytesMut::new();
            let error = ColumnData::String(Some("€".into()))
                .encode(&mut BytesMutWithTypeInfo::new(&mut encoded).with_type_info(&type_info))
                .unwrap_err();
            assert!(matches!(
                error,
                crate::Error::Encoding(message) if message == "unrepresentable character"
            ));
        }
    }
}

#[tokio::test]
async fn legacy_codepages_preserve_empty_and_null_values() {
    for sort_id in [30, 49] {
        for ty in [
            VarLenType::BigChar,
            VarLenType::BigVarChar,
            VarLenType::Text,
        ] {
            let type_info = TypeInfo::VarLenSized(VarLenContext::new(
                ty,
                32,
                Some(Collation::new(0x409, sort_id)),
            ));
            let mut reader = wire_value(ty, b"").into_sql_read_bytes();
            let empty = ColumnData::decode(&mut reader, &type_info).await.unwrap();
            assert_eq!(empty, ColumnData::String(Some("".into())));
            let null: &[u8] = if ty == VarLenType::Text {
                &[0]
            } else {
                &[255, 255]
            };
            let mut reader = BytesMut::from(null).into_sql_read_bytes();
            let null = ColumnData::decode(&mut reader, &type_info).await.unwrap();
            assert_eq!(null, ColumnData::String(None));
        }
    }
}

#[test]
fn legacy_codepages_bulk_limits_use_encoded_byte_length() {
    for sort_id in [30, 49] {
        for ty in [VarLenType::BigChar, VarLenType::BigVarChar] {
            let type_info = TypeInfo::VarLenSized(VarLenContext::new(
                ty,
                1,
                Some(Collation::new(0x409, sort_id)),
            ));
            let mut encoded = BytesMut::new();
            ColumnData::String(Some("é".into()))
                .encode(&mut BytesMutWithTypeInfo::new(&mut encoded).with_type_info(&type_info))
                .unwrap();
            assert_eq!(&encoded[..], &[1, 0, 0x82]);
            let error = ColumnData::String(Some("éé".into()))
                .encode(&mut BytesMutWithTypeInfo::new(&mut encoded).with_type_info(&type_info))
                .unwrap_err();
            assert!(matches!(error, crate::Error::BulkInput(_)));
        }
    }
}

#[test]
fn legacy_codepages_preserve_builtin_and_unknown_collations() {
    let windows = Collation::new(0x409, 52);
    assert_eq!(windows.encoding().unwrap(), encoding_rs::WINDOWS_1252);
    assert_eq!(windows.codec().unwrap().decode(b"caf\xe9").unwrap(), "café");
    assert_eq!(windows.codec().unwrap().encode("café").unwrap(), b"caf\xe9");
    assert!(Collation::new(0x409, 62).codec().is_err());
    assert_eq!(Collation::new(0x409, 30).to_string(), "CP437");
    assert_eq!(Collation::new(0x409, 49).to_string(), "CP850");
}

#[tokio::test]
async fn legacy_codepages_decode_sql_variant_char_and_varchar() {
    for sort_id in [30, 49] {
        for ty in [VarLenType::BigChar, VarLenType::BigVarChar] {
            let mut wire = BytesMut::new();
            wire.put_u32_le(13);
            wire.put_u8(ty as u8);
            wire.put_u8(7);
            wire.put_u32_le(0x409);
            wire.put_u8(sort_id);
            wire.put_u16_le(4);
            wire.extend_from_slice(b"Caf\x82");
            let type_info =
                TypeInfo::VarLenSized(VarLenContext::new(VarLenType::SSVariant, 8016, None));
            let mut reader = wire.into_sql_read_bytes();
            let decoded = ColumnData::decode(&mut reader, &type_info).await.unwrap();
            assert_eq!(decoded, ColumnData::String(Some("Café".into())));
        }
    }
}
