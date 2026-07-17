//! Auto-generated module
//!
//! 🤖 Generated with [SplitRS](https://github.com/cool-japan/splitrs)

// https://github.com/sqlite/sqlite/blob/80511f32f7e71062026edd471913ef0455563964/src/select.c#L197-L257
bitflags::bitflags! {
    /// `JOIN` types
    #[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
    pub struct JoinType: u8 {
        /// `INNER`
        const INNER   = 0x01;
        /// `CROSS` => INNER|CROSS
        const CROSS   = 0x02;
        /// `NATURAL`
        const NATURAL = 0x04;
        /// `LEFT` => LEFT|OUTER
        const LEFT    = 0x08;
        /// `RIGHT` => RIGHT|OUTER
        const RIGHT   = 0x10;
        /// `OUTER`
        const OUTER   = 0x20;
    }
}

bitflags::bitflags! {
    /// `CREATE TABLE` options
    #[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
    pub struct TableOptions: u8 {
        /// None
        const NONE = 0;
        /// `WITHOUT ROWID`
        const WITHOUT_ROWID = 1;
        /// `STRICT`
        const STRICT = 2;
    }
}
