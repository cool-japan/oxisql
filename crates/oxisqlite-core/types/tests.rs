//! Tests for the `types` module.

use super::*;
use crate::storage::sqlite3_ondisk::{read_record, read_varint};

/// Converts a decoded [`RefValue`] into an owned [`Value`] so decoded
/// output can be compared against the original input with `assert_eq!`.
fn owned_value_of(rv: &RefValue) -> Value {
    match rv {
        RefValue::Null => Value::Null,
        RefValue::Integer(i) => Value::Integer(*i),
        RefValue::Float(f) => Value::Float(*f),
        RefValue::Text(t) => Value::Text(Text {
            value: t.value.to_slice().to_vec(),
            subtype: t.subtype.clone(),
        }),
        RefValue::Blob(b) => Value::Blob(b.to_slice().to_vec()),
    }
}

/// Parses `payload` with the crate's own record reader ([`read_record`])
/// and returns the on-disk header size (decoded from the payload's own
/// length-prefix varint) plus the decoded values. Round-tripping through
/// the real reader is the strongest available check that the bytes are
/// spec-correct SQLite record format, not just "didn't panic".
fn round_trip_payload(payload: &[u8], value_count: usize) -> (usize, Vec<Value>) {
    let (header_size, _) =
        read_varint(payload).expect("payload must start with a valid header-length varint");
    let mut decoded = ImmutableRecord::new(payload.len(), value_count);
    read_record(payload, &mut decoded).expect("payload must parse back as a valid record");
    let values = decoded.get_values().iter().map(owned_value_of).collect();
    (header_size as usize, values)
}

/// Round-trips `values` through [`ImmutableRecord::from_registers`].
fn round_trip_from_registers(values: &[Value]) -> (usize, Vec<Value>) {
    let registers: Vec<Register> = values.iter().cloned().map(Register::Value).collect();
    let record = ImmutableRecord::from_registers(&registers);
    round_trip_payload(record.get_payload(), values.len())
}

/// Round-trips `values` through `Record::serialize`, the other call
/// site sharing `resolve_big_header_size`.
fn round_trip_serialize(values: &[Value]) -> (usize, Vec<Value>) {
    let record = Record::new(values.to_vec());
    let mut buf = Vec::new();
    record.serialize(&mut buf);
    round_trip_payload(&buf, values.len())
}

#[test]
fn test_serialize_null() {
    let record = Record::new(vec![Value::Null]);
    let mut buf = Vec::new();
    record.serialize(&mut buf);

    let header_length = record.values.len() + 1;
    let header = &buf[0..header_length];
    // First byte should be header size
    assert_eq!(header[0], header_length as u8);
    // Second byte should be serial type for NULL
    assert_eq!(header[1] as u64, u64::from(SerialType::null()));
    // Check that the buffer is empty after the header
    assert_eq!(buf.len(), header_length);
}

#[test]
fn test_serialize_integers() {
    let record = Record::new(vec![
        Value::Integer(0),                 // Should use ConstInt0
        Value::Integer(1),                 // Should use ConstInt1
        Value::Integer(42),                // Should use SERIAL_TYPE_I8
        Value::Integer(1000),              // Should use SERIAL_TYPE_I16
        Value::Integer(1_000_000),         // Should use SERIAL_TYPE_I24
        Value::Integer(1_000_000_000),     // Should use SERIAL_TYPE_I32
        Value::Integer(1_000_000_000_000), // Should use SERIAL_TYPE_I48
        Value::Integer(i64::MAX),          // Should use SERIAL_TYPE_I64
    ]);
    let mut buf = Vec::new();
    record.serialize(&mut buf);

    let header_length = record.values.len() + 1;
    let header = &buf[0..header_length];
    // First byte should be header size
    assert_eq!(header[0], header_length as u8); // Header should be larger than number of values

    // Check that correct serial types were chosen
    assert_eq!(header[1] as u64, u64::from(SerialType::const_int0())); // 8
    assert_eq!(header[2] as u64, u64::from(SerialType::const_int1())); // 9
    assert_eq!(header[3] as u64, u64::from(SerialType::i8())); // 1
    assert_eq!(header[4] as u64, u64::from(SerialType::i16())); // 2
    assert_eq!(header[5] as u64, u64::from(SerialType::i24())); // 3
    assert_eq!(header[6] as u64, u64::from(SerialType::i32())); // 4
    assert_eq!(header[7] as u64, u64::from(SerialType::i48())); // 5
    assert_eq!(header[8] as u64, u64::from(SerialType::i64())); // 6

    // test that the bytes after the header can be interpreted as the correct values
    let mut cur_offset = header_length;

    // Value::Integer(0) - ConstInt0: NO PAYLOAD BYTES
    // Value::Integer(1) - ConstInt1: NO PAYLOAD BYTES

    // Value::Integer(42) - I8: 1 byte
    let i8_bytes = &buf[cur_offset..cur_offset + size_of::<i8>()];
    cur_offset += size_of::<i8>();

    // Value::Integer(1000) - I16: 2 bytes
    let i16_bytes = &buf[cur_offset..cur_offset + size_of::<i16>()];
    cur_offset += size_of::<i16>();

    // Value::Integer(1_000_000) - I24: 3 bytes
    let i24_bytes = &buf[cur_offset..cur_offset + 3];
    cur_offset += 3;

    // Value::Integer(1_000_000_000) - I32: 4 bytes
    let i32_bytes = &buf[cur_offset..cur_offset + size_of::<i32>()];
    cur_offset += size_of::<i32>();

    // Value::Integer(1_000_000_000_000) - I48: 6 bytes
    let i48_bytes = &buf[cur_offset..cur_offset + 6];
    cur_offset += 6;

    // Value::Integer(i64::MAX) - I64: 8 bytes
    let i64_bytes = &buf[cur_offset..cur_offset + size_of::<i64>()];

    // Verify the payload values
    let val_int8 = i8::from_be_bytes(i8_bytes.try_into().unwrap());
    let val_int16 = i16::from_be_bytes(i16_bytes.try_into().unwrap());

    let mut i24_with_padding = vec![0];
    i24_with_padding.extend(i24_bytes);
    let val_int24 = i32::from_be_bytes(i24_with_padding.try_into().unwrap());

    let val_int32 = i32::from_be_bytes(i32_bytes.try_into().unwrap());

    let mut i48_with_padding = vec![0, 0];
    i48_with_padding.extend(i48_bytes);
    let val_int48 = i64::from_be_bytes(i48_with_padding.try_into().unwrap());

    let val_int64 = i64::from_be_bytes(i64_bytes.try_into().unwrap());

    assert_eq!(val_int8, 42);
    assert_eq!(val_int16, 1000);
    assert_eq!(val_int24, 1_000_000);
    assert_eq!(val_int32, 1_000_000_000);
    assert_eq!(val_int48, 1_000_000_000_000);
    assert_eq!(val_int64, i64::MAX);

    //Size of buffer = header + payload bytes
    // ConstInt0 and ConstInt1 contribute 0 bytes to payload
    assert_eq!(
        buf.len(),
        header_length  // 9 bytes (header size + 8 serial types)
            + 0        // ConstInt0: 0 bytes
            + 0        // ConstInt1: 0 bytes  
            + size_of::<i8>()        // I8: 1 byte
            + size_of::<i16>()        // I16: 2 bytes
            + (size_of::<i32>() - 1)        // I24: 3 bytes
            + size_of::<i32>()        // I32: 4 bytes
            + (size_of::<i64>() - 2)        // I48: 6 bytes
            + size_of::<i64>() // I64: 8 bytes
    );
}

#[test]
fn test_serialize_const_integers() {
    let record = Record::new(vec![Value::Integer(0), Value::Integer(1)]);
    let mut buf = Vec::new();
    record.serialize(&mut buf);

    // [header_size, serial_type_0, serial_type_1] + no payload bytes
    let expected_header_size = 3; // 1 byte for header size + 2 bytes for serial types

    assert_eq!(buf.len(), expected_header_size);

    // Check header size
    assert_eq!(buf[0], expected_header_size as u8);

    assert_eq!(buf[1] as u64, u64::from(SerialType::const_int0())); // Should be 8
    assert_eq!(buf[2] as u64, u64::from(SerialType::const_int1())); // Should be 9

    assert_eq!(buf[1], 8); // ConstInt0 serial type
    assert_eq!(buf[2], 9); // ConstInt1 serial type
}

#[test]
fn test_serialize_single_const_int0() {
    let record = Record::new(vec![Value::Integer(0)]);
    let mut buf = Vec::new();
    record.serialize(&mut buf);

    // Expected: [header_size=2, serial_type=8]
    assert_eq!(buf.len(), 2);
    assert_eq!(buf[0], 2); // Header size
    assert_eq!(buf[1], 8); // ConstInt0 serial type
}

#[test]
fn test_serialize_float() {
    #[warn(clippy::approx_constant)]
    let record = Record::new(vec![Value::Float(3.15555)]);
    let mut buf = Vec::new();
    record.serialize(&mut buf);

    let header_length = record.values.len() + 1;
    let header = &buf[0..header_length];
    // First byte should be header size
    assert_eq!(header[0], header_length as u8);
    // Second byte should be serial type for FLOAT
    assert_eq!(header[1] as u64, u64::from(SerialType::f64()));
    // Check that the bytes after the header can be interpreted as the float
    let float_bytes = &buf[header_length..header_length + size_of::<f64>()];
    let float = f64::from_be_bytes(float_bytes.try_into().unwrap());
    assert_eq!(float, 3.15555);
    // Check that buffer length is correct
    assert_eq!(buf.len(), header_length + size_of::<f64>());
}

#[test]
fn test_serialize_text() {
    let text = "hello";
    let record = Record::new(vec![Value::Text(Text::new(text))]);
    let mut buf = Vec::new();
    record.serialize(&mut buf);

    let header_length = record.values.len() + 1;
    let header = &buf[0..header_length];
    // First byte should be header size
    assert_eq!(header[0], header_length as u8);
    // Second byte should be serial type for TEXT, which is (len * 2 + 13)
    assert_eq!(header[1], (5 * 2 + 13) as u8);
    // Check the actual text bytes
    assert_eq!(&buf[2..7], b"hello");
    // Check that buffer length is correct
    assert_eq!(buf.len(), header_length + text.len());
}

#[test]
fn test_serialize_blob() {
    let blob = vec![1, 2, 3, 4, 5];
    let record = Record::new(vec![Value::Blob(blob.clone())]);
    let mut buf = Vec::new();
    record.serialize(&mut buf);

    let header_length = record.values.len() + 1;
    let header = &buf[0..header_length];
    // First byte should be header size
    assert_eq!(header[0], header_length as u8);
    // Second byte should be serial type for BLOB, which is (len * 2 + 12)
    assert_eq!(header[1], (5 * 2 + 12) as u8);
    // Check the actual blob bytes
    assert_eq!(&buf[2..7], &[1, 2, 3, 4, 5]);
    // Check that buffer length is correct
    assert_eq!(buf.len(), header_length + blob.len());
}

#[test]
fn test_serialize_mixed_types() {
    let text = "test";
    let record = Record::new(vec![
        Value::Null,
        Value::Integer(42),
        Value::Float(3.15),
        Value::Text(Text::new(text)),
    ]);
    let mut buf = Vec::new();
    record.serialize(&mut buf);

    let header_length = record.values.len() + 1;
    let header = &buf[0..header_length];
    // First byte should be header size
    assert_eq!(header[0], header_length as u8);
    // Second byte should be serial type for NULL
    assert_eq!(header[1] as u64, u64::from(SerialType::null()));
    // Third byte should be serial type for I8
    assert_eq!(header[2] as u64, u64::from(SerialType::i8()));
    // Fourth byte should be serial type for F64
    assert_eq!(header[3] as u64, u64::from(SerialType::f64()));
    // Fifth byte should be serial type for TEXT, which is (len * 2 + 13)
    assert_eq!(header[4] as u64, (4 * 2 + 13) as u64);

    // Check that the bytes after the header can be interpreted as the correct values
    let mut cur_offset = header_length;
    let i8_bytes = &buf[cur_offset..cur_offset + size_of::<i8>()];
    cur_offset += size_of::<i8>();
    let f64_bytes = &buf[cur_offset..cur_offset + size_of::<f64>()];
    cur_offset += size_of::<f64>();
    let text_bytes = &buf[cur_offset..cur_offset + text.len()];

    let val_int8 = i8::from_be_bytes(i8_bytes.try_into().unwrap());
    let val_float = f64::from_be_bytes(f64_bytes.try_into().unwrap());
    let val_text = String::from_utf8(text_bytes.to_vec()).unwrap();

    assert_eq!(val_int8, 42);
    assert_eq!(val_float, 3.15);
    assert_eq!(val_text, "test");

    // Check that buffer length is correct
    assert_eq!(
        buf.len(),
        header_length + size_of::<i8>() + size_of::<f64>() + text.len()
    );
}

// Big-header (> 126 bytes) fixup tests: `resolve_big_header_size` covers
// a growing header-length varint pushing the header size past another
// varint-length boundary. Getting this wrong either panics (the old
// `todo!()`) or silently corrupts the record.

#[test]
fn test_from_registers_wide_table_round_trip() {
    // 150 columns: comfortably past the ~127-column threshold where the
    // sum of per-column serial-type varints alone exceeds 126 bytes.
    // Before this fix, building this record panicked via
    // `todo!("calculate big header size extra bytes")`.
    const NUM_COLUMNS: usize = 150;
    let values: Vec<Value> = (0..NUM_COLUMNS)
        .map(|i| match i % 5 {
            0 => Value::Null,
            1 => Value::Integer(i as i64),
            2 => Value::Float(i as f64 * 1.5),
            3 => Value::Text(Text::new(&format!("col{i}"))),
            _ => Value::Blob(vec![(i % 256) as u8; 3]),
        })
        .collect();

    let (header_size, round_tripped) = round_trip_from_registers(&values);
    assert!(header_size > 126, "got header_size = {header_size}");
    assert_eq!(round_tripped, values);
}

#[test]
fn test_serialize_large_text_column_round_trip() {
    // 125 single-byte filler columns plus one 10KB+ TEXT column: it's
    // the *content* of that one column - not column count (126 total,
    // fewer than the 127 needed above) - that tips the header past 126
    // bytes. A text serial type encodes `len * 2 + 13`, which needs 3
    // bytes once the string crosses a few KB, instead of 1.
    const NUM_FILLERS: usize = 125;
    let mut values: Vec<Value> = (0..NUM_FILLERS)
        .map(|i| Value::Integer((i % 2) as i64))
        .collect();
    values.push(Value::Text(Text::new(&"x".repeat(10_000))));
    assert!(values.len() < 127);

    let (header_size, round_tripped) = round_trip_serialize(&values);
    assert!(header_size > 126, "got header_size = {header_size}");
    assert_eq!(round_tripped, values);
}

#[test]
fn test_big_header_length_prefix_crosses_its_own_varint_boundary() {
    // Trickiest edge case: exactly 127 NULL columns, each contributing
    // exactly 1 byte (serial type 0), so the tentative header size is
    // exactly 127 - one over the common-case threshold. A naive fixup
    // (mimicking the common case) would add 1 byte -> 128, but encoding
    // 128 needs *2* bytes, not 1, so that guess would be
    // self-inconsistent. The correct fixed point is 127 + 2 = 129,
    // which does fit in 2 bytes.
    const NUM_COLUMNS: usize = 127;
    let values: Vec<Value> = vec![Value::Null; NUM_COLUMNS];
    let registers: Vec<Register> = values.iter().cloned().map(Register::Value).collect();
    let record = ImmutableRecord::from_registers(&registers);
    let payload = record.get_payload();

    // 2-byte length-prefix varint + 127 one-byte serial types (NULL) +
    // 0 value bytes (NULL stores no content) = 129. 129 as a SQLite
    // varint (7 bits/byte, MSB = continuation, big-endian) is 0x81 0x01.
    assert_eq!(payload.len(), 129);
    assert_eq!(&payload[0..2], &[0x81, 0x01]);
    assert!(payload[2..129].iter().all(|&b| b == 0));

    let (header_size, round_tripped) = round_trip_payload(payload, values.len());
    assert_eq!(header_size, 129);
    assert_eq!(round_tripped, values);

    // `Record::serialize` shares the same fixup helper - confirm it
    // produces byte-identical output for the same logical row.
    let mut buf = Vec::new();
    Record::new(values.clone()).serialize(&mut buf);
    assert_eq!(buf, payload);
}

#[test]
fn test_small_header_regression() {
    // Ordinary columns well within the pre-existing common-case path
    // (header <= 126 bytes), via both call sites sharing the fixup
    // helper; must behave exactly as before the fix.
    let values = vec![
        Value::Null,
        Value::Integer(42),
        Value::Float(2.5),
        Value::Text(Text::new("hello")),
        Value::Blob(vec![1, 2, 3, 4, 5]),
    ];
    // 5 one-byte serial types + 1-byte header-length prefix.
    let (from_registers_header_size, from_registers_values) = round_trip_from_registers(&values);
    assert_eq!(from_registers_header_size, 6);
    assert_eq!(from_registers_values, values);

    let (serialize_header_size, serialize_values) = round_trip_serialize(&values);
    assert_eq!(serialize_header_size, 6);
    assert_eq!(serialize_values, values);
}
