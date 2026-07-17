//! Auto-generated test module (consolidated from inline `#[cfg(test)] mod` blocks)

use crate::types::SerialType;

use super::*;

#[cfg(test)]
mod tests_2 {
    use crate::Value;

    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case(&[], SerialType::null(), Value::Null)]
    #[case(&[255], SerialType::i8(), Value::Integer(-1))]
    #[case(&[0x12, 0x34], SerialType::i16(), Value::Integer(0x1234))]
    #[case(&[0xFE], SerialType::i8(), Value::Integer(-2))]
    #[case(&[0x12, 0x34, 0x56], SerialType::i24(), Value::Integer(0x123456))]
    #[case(&[0x12, 0x34, 0x56, 0x78], SerialType::i32(), Value::Integer(0x12345678))]
    #[case(&[0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC], SerialType::i48(), Value::Integer(0x123456789ABC))]
    #[case(&[0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC, 0xDE, 0xFF], SerialType::i64(), Value::Integer(0x123456789ABCDEFF))]
    #[case(&[0x40, 0x09, 0x21, 0xFB, 0x54, 0x44, 0x2D, 0x18], SerialType::f64(), Value::Float(std::f64::consts::PI))]
    #[case(&[1, 2], SerialType::const_int0(), Value::Integer(0))]
    #[case(&[65, 66], SerialType::const_int1(), Value::Integer(1))]
    #[case(&[1, 2, 3], SerialType::blob(3), Value::Blob(vec![1, 2, 3].into()))]
    #[case(&[], SerialType::blob(0), Value::Blob(vec![].into()))] // empty blob
    #[case(&[65, 66, 67], SerialType::text(3), Value::build_text("ABC"))]
    #[case(&[0x80], SerialType::i8(), Value::Integer(-128))]
    #[case(&[0x80, 0], SerialType::i16(), Value::Integer(-32768))]
    #[case(&[0x80, 0, 0], SerialType::i24(), Value::Integer(-8388608))]
    #[case(&[0x80, 0, 0, 0], SerialType::i32(), Value::Integer(-2147483648))]
    #[case(&[0x80, 0, 0, 0, 0, 0], SerialType::i48(), Value::Integer(-140737488355328))]
    #[case(&[0x80, 0, 0, 0, 0, 0, 0, 0], SerialType::i64(), Value::Integer(-9223372036854775808))]
    #[case(&[0x7f], SerialType::i8(), Value::Integer(127))]
    #[case(&[0x7f, 0xff], SerialType::i16(), Value::Integer(32767))]
    #[case(&[0x7f, 0xff, 0xff], SerialType::i24(), Value::Integer(8388607))]
    #[case(&[0x7f, 0xff, 0xff, 0xff], SerialType::i32(), Value::Integer(2147483647))]
    #[case(&[0x7f, 0xff, 0xff, 0xff, 0xff, 0xff], SerialType::i48(), Value::Integer(140737488355327))]
    #[case(&[0x7f, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff], SerialType::i64(), Value::Integer(9223372036854775807))]
    fn test_read_value(
        #[case] buf: &[u8],
        #[case] serial_type: SerialType,
        #[case] expected: Value,
    ) {
        let result = read_value(buf, serial_type).unwrap();
        assert_eq!(result.0.to_owned(), expected);
    }

    #[test]
    fn test_serial_type_helpers() {
        assert_eq!(
            TryInto::<SerialType>::try_into(12u64).unwrap(),
            SerialType::blob(0)
        );
        assert_eq!(
            TryInto::<SerialType>::try_into(14u64).unwrap(),
            SerialType::blob(1)
        );
        assert_eq!(
            TryInto::<SerialType>::try_into(13u64).unwrap(),
            SerialType::text(0)
        );
        assert_eq!(
            TryInto::<SerialType>::try_into(15u64).unwrap(),
            SerialType::text(1)
        );
        assert_eq!(
            TryInto::<SerialType>::try_into(16u64).unwrap(),
            SerialType::blob(2)
        );
        assert_eq!(
            TryInto::<SerialType>::try_into(17u64).unwrap(),
            SerialType::text(2)
        );
    }

    #[rstest]
    #[case(0, SerialType::null())]
    #[case(1, SerialType::i8())]
    #[case(2, SerialType::i16())]
    #[case(3, SerialType::i24())]
    #[case(4, SerialType::i32())]
    #[case(5, SerialType::i48())]
    #[case(6, SerialType::i64())]
    #[case(7, SerialType::f64())]
    #[case(8, SerialType::const_int0())]
    #[case(9, SerialType::const_int1())]
    #[case(12, SerialType::blob(0))]
    #[case(13, SerialType::text(0))]
    #[case(14, SerialType::blob(1))]
    #[case(15, SerialType::text(1))]
    fn test_parse_serial_type(#[case] input: u64, #[case] expected: SerialType) {
        let result = SerialType::try_from(input).unwrap();
        assert_eq!(result, expected);
    }

    #[test]
    fn test_validate_serial_type() {
        for i in 0..=9 {
            let result = validate_serial_type(i);
            assert!(result.is_ok());
        }
        for i in 10..=11 {
            let result = validate_serial_type(i);
            assert!(result.is_err());
        }
        for i in 12..=1000 {
            let result = validate_serial_type(i);
            assert!(result.is_ok());
        }
    }

    #[test]
    fn test_smallvec_iter() {
        let mut small_vec = SmallVec::<i32, 4>::new();
        (0..8).for_each(|i| small_vec.push(i));

        let mut iter = small_vec.iter();
        assert_eq!(iter.next(), Some(0));
        assert_eq!(iter.next(), Some(1));
        assert_eq!(iter.next(), Some(2));
        assert_eq!(iter.next(), Some(3));
        assert_eq!(iter.next(), Some(4));
        assert_eq!(iter.next(), Some(5));
        assert_eq!(iter.next(), Some(6));
        assert_eq!(iter.next(), Some(7));
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn test_smallvec_get() {
        let mut small_vec = SmallVec::<i32, 4>::new();
        (0..8).for_each(|i| small_vec.push(i));

        (0..8).for_each(|i| {
            assert_eq!(small_vec.get(i), Some(i as i32));
        });

        assert_eq!(small_vec.get(8), None);
    }

    // ---------------------------------------------------------------------
    // Corrupt/truncated on-disk input regression tests.
    //
    // Each of these feeds deliberately malformed bytes to a low-level parser
    // routine and asserts it returns `LimboError::Corrupt(_)` instead of
    // panicking (out-of-bounds index, subtraction underflow, or a live
    // `assert!`). They FAIL (panic) against the pre-hardening code.
    // ---------------------------------------------------------------------

    /// A 9-byte varint whose 9th byte is missing: the first eight bytes all
    /// have their continuation bit set, then the buffer ends. The old code
    /// indexed `buf[8]` unconditionally after the `0..8` loop.
    #[test]
    fn test_read_varint_truncated_ninth_byte_is_corrupt() {
        let buf = [0x80u8; 8];
        let result = read_varint(&buf);
        assert!(
            matches!(result, Err(LimboError::Corrupt(_))),
            "expected Corrupt for a truncated 9-byte varint, got {:?}",
            result
        );
    }

    /// `read_btree_cell` must reject an attacker-supplied cell pointer that
    /// sits past the end of the page instead of indexing out of bounds or
    /// underflowing `page.len() - pos`.
    #[test]
    fn test_read_btree_cell_rejects_out_of_range_pos() {
        static PAGE: [u8; 16] = [0u8; 16];
        for page_type in [
            PageType::TableLeaf,
            PageType::IndexLeaf,
            PageType::TableInterior,
            PageType::IndexInterior,
        ] {
            // `pos` well past the 16-byte page.
            let result = read_btree_cell(&PAGE, &page_type, 1000, 4096, 0, 4096);
            assert!(
                matches!(result, Err(LimboError::Corrupt(_))),
                "expected Corrupt for out-of-range pos on {:?}, got {:?}",
                page_type,
                result
            );
        }
        // An interior cell pointer that leaves fewer than the 4 bytes needed
        // for the left-child pointer must also be rejected.
        let result = read_btree_cell(&PAGE, &PageType::TableInterior, 14, 4096, 0, 4096);
        assert!(
            matches!(result, Err(LimboError::Corrupt(_))),
            "expected Corrupt for truncated interior left-child pointer, got {:?}",
            result
        );
    }

    /// `read_btree_cell` must reject a cell whose declared payload overflows
    /// (so its on-page length is computed from the payload size, not from the
    /// bytes actually left in the page) and thus extends past a truncated page
    /// buffer, instead of slicing `&page[pos..pos + to_read]` out of bounds.
    #[test]
    fn test_read_btree_cell_rejects_overflow_payload_beyond_page() {
        let max_local = 4096 - 35;
        let min_local = ((4096 - 12) * 32 / 255) - 23;
        // A 64-byte (truncated) page. `payload_size = 5000` is encoded as the
        // 2-byte varint [0xA7, 0x08]; 5000 > max_local, so the overflow branch
        // fires and `to_read` (~912) runs well past the 64-byte buffer.
        // (`read_btree_cell` takes `&'static [u8]`, hence the static buffers.)
        // Table-leaf: payload varint at pos 0, then a 1-byte rowid varint.
        static TABLE_LEAF: [u8; 64] = {
            let mut a = [0u8; 64];
            a[0] = 0xA7;
            a[1] = 0x08;
            a[2] = 0x01;
            a
        };
        let result = read_btree_cell(
            &TABLE_LEAF,
            &PageType::TableLeaf,
            0,
            max_local,
            min_local,
            4096,
        );
        assert!(
            matches!(result, Err(LimboError::Corrupt(_))),
            "expected Corrupt for overflowing table-leaf payload, got {:?}",
            result
        );
        // Index-leaf: payload varint at pos 0.
        static INDEX_LEAF: [u8; 64] = {
            let mut a = [0u8; 64];
            a[0] = 0xA7;
            a[1] = 0x08;
            a
        };
        let result = read_btree_cell(
            &INDEX_LEAF,
            &PageType::IndexLeaf,
            0,
            max_local,
            min_local,
            4096,
        );
        assert!(
            matches!(result, Err(LimboError::Corrupt(_))),
            "expected Corrupt for overflowing index-leaf payload, got {:?}",
            result
        );
        // Index-interior: 4-byte left-child pointer, then the payload varint.
        static INDEX_INTERIOR: [u8; 64] = {
            let mut a = [0u8; 64];
            a[4] = 0xA7;
            a[5] = 0x08;
            a
        };
        let result = read_btree_cell(
            &INDEX_INTERIOR,
            &PageType::IndexInterior,
            0,
            max_local,
            min_local,
            4096,
        );
        assert!(
            matches!(result, Err(LimboError::Corrupt(_))),
            "expected Corrupt for overflowing index-interior payload, got {:?}",
            result
        );
    }

    /// `read_payload` must not underflow `cell_len - 4` when an overflowing
    /// cell leaves fewer than 4 trailing bytes for the overflow page number.
    #[test]
    fn test_read_payload_rejects_short_overflow_pointer() {
        static SHORT: [u8; 2] = [0xAA, 0xBB];
        // payload_size (100) > available bytes (2) => overflow branch, but
        // there are fewer than 4 bytes for the overflow page pointer.
        let result = read_payload(&SHORT, 100);
        assert!(
            matches!(result, Err(LimboError::Corrupt(_))),
            "expected Corrupt for a short overflow-page pointer, got {:?}",
            result
        );
    }

    /// `read_record` must reject a header whose declared size is smaller than
    /// the varint that encoded it (would underflow), instead of `assert!`ing.
    #[test]
    fn test_read_record_rejects_header_smaller_than_prefix() {
        // Single byte 0x00: header_size == 0, but the size varint itself is 1
        // byte, so `header_size - nr` would underflow.
        let payload = [0x00u8];
        let mut reuse = crate::types::ImmutableRecord::new(16, 4);
        let result = read_record(&payload, &mut reuse);
        assert!(
            matches!(result, Err(LimboError::Corrupt(_))),
            "expected Corrupt for an under-sized record header, got {:?}",
            result
        );
    }

    /// `read_record` must also reject a header that under-counts the bytes a
    /// serial-type varint actually consumes (the in-loop guard).
    #[test]
    fn test_read_record_rejects_serial_type_overrunning_header() {
        // header_size varint = 2 (1 leftover header byte), but the serial type
        // is a 2-byte varint (0x81 0x00), overrunning the 1 remaining byte.
        let payload = [0x02u8, 0x81, 0x00];
        let mut reuse = crate::types::ImmutableRecord::new(16, 4);
        let result = read_record(&payload, &mut reuse);
        assert!(
            matches!(result, Err(LimboError::Corrupt(_))),
            "expected Corrupt for a serial type overrunning the header, got {:?}",
            result
        );
    }
}
