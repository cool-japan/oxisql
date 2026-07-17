# oxisqlite extension API

The extension API of the C-free **oxisqlite** engine — a Pure-Rust fork of
[limbo](https://github.com/tursodatabase/limbo) 0.0.22, internal to the OxiSQL
workspace.

This crate lets you extend the oxisqlite engine with new functionality written
in ergonomic, **pure Rust**, in the spirit of traditional `sqlite3` extensions
but without any C. You define your extension and register it with the
`register_extension!` macro.

- **Role:** extension API (scalar / aggregate functions, virtual tables, VFS).
- **Version:** 0.3.3 (2026-07-17).
- **Tests:** 0 dedicated `nextest` unit tests in this crate; validated
  indirectly through `oxisqlite-core`'s integration test suite, which depends
  on both `oxisqlite-ext` and `oxisqlite-macros` (verified 2026-07-17).
- **Approx LOC:** ~1,134 (tokei `src/`).
- **Pure Rust / no C:** 100% Rust. **No C allocator**, no C parser generator, no
  `cc` / `build.rs`, no global-allocator injection. `CC=/usr/bin/false cargo
  build` succeeds.
- **Internal:** private member of the OxiSQL workspace; not published separately.

## Supported extension points

- **Scalar functions** — via the `scalar` macro.
- **Aggregate functions** — via the `AggregateDerive` macro and the `AggFunc`
  trait.
- **Virtual tables** — via the `VTabModuleDerive` macro and the `VTabModule` /
  `VTable` / `VTabCursor` traits. `CREATE VIRTUAL TABLE` resolves column names
  from the single, live module instance `xCreate` already returns; fixed in
  0.3.3 to no longer perform a wasteful (and, for a module with side effects,
  unsound) extra create-then-destroy round trip just to learn the columns.
- **VFS modules** — by implementing the `VfsExtension` and `VfsFile` traits
  (requires the `vfs` feature).

## Registering an extension

Extensions are wired into the engine with the `register_extension!` macro:

```rust
register_extension! {
    scalars: { double },        // names of your scalar functions
    aggregates: { Percentile },
    vtabs: { CsvVTableModule },
    vfs: { ExampleFS },
}
```

> **Note:** any derive macro from this crate must currently be used in the same
> file as the `register_extension!` invocation.

## Scalar example

Annotate a function with the `scalar` macro, giving the SQL-callable name (and
an optional alias), e.g. `SELECT double(4);` or `SELECT twice(4);`.

```rust
use oxisqlite_ext::{register_extension, scalar, Value, ValueType};

#[scalar(name = "double", alias = "twice")]
fn double(&self, args: &[Value]) -> Value {
    if let Some(arg) = args.first() {
        match arg.value_type() {
            ValueType::Float => Value::from_float(arg.to_float().unwrap_or(0.0) * 2.0),
            ValueType::Integer => Value::from_integer(arg.to_integer().unwrap_or(0) * 2),
            _ => Value::null(),
        }
    } else {
        Value::null()
    }
}
```

## Aggregate example

Derive `AggregateDerive` on a struct and implement `AggFunc`, e.g.
`SELECT percentile(value, 40);`.

```rust
use oxisqlite_ext::{AggregateDerive, AggFunc, Value};

#[derive(AggregateDerive)]
struct Percentile;

impl AggFunc for Percentile {
    /// State tracked across the rows of a group.
    type State = (Vec<f64>, Option<f64>, Option<String>);
    /// Error type (must implement `Display`).
    type Error = String;

    const NAME: &'static str = "percentile";
    const ARGS: i32 = 2;

    /// Called once per row in the group.
    fn step(state: &mut Self::State, args: &[Value]) {
        let (values, p_value, error) = state;
        if let (Some(y), Some(p)) = (
            args.first().and_then(Value::to_float),
            args.get(1).and_then(Value::to_float),
        ) {
            if !(0.0..=100.0).contains(&p) {
                *error = Some("Percentile P must be between 0 and 100.".to_string());
                return;
            }
            match *p_value {
                Some(existing_p) if (existing_p - p).abs() >= 0.001 => {
                    *error = Some("P values must remain consistent.".to_string());
                    return;
                }
                None => *p_value = Some(p),
                _ => {}
            }
            values.push(y);
        }
    }

    /// Reduce the accumulated state to a single result (or an error).
    fn finalize(state: Self::State) -> Result<Value, Self::Error> {
        let (mut values, p_value, error) = state;
        if let Some(error) = error {
            return Err(error);
        }
        if values.is_empty() {
            return Ok(Value::null());
        }
        values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let n = values.len() as f64;
        let p = p_value.unwrap_or(0.0);
        let index = (p * (n - 1.0) / 100.0).floor() as usize;
        Ok(Value::from_float(values[index]))
    }
}
```

## Virtual table example

A virtual table is a module (`VTabModuleDerive` + `VTabModule`) that yields a
`VTable`, which in turn opens a `VTabCursor`.

```rust
use oxisqlite_ext::{
    VTabModuleDerive, VTabModule, VTable, VTabCursor, VTabKind, Value, ResultCode,
};

#[derive(Debug, VTabModuleDerive)]
struct CsvVTableModule;

impl VTabModule for CsvVTableModule {
    type Table = CsvTable;
    const NAME: &'static str = "csv_data";
    const VTAB_KIND: VTabKind = VTabKind::VirtualTable;

    /// Declare the virtual table and its schema.
    fn create(_args: &[Value]) -> Result<(String, Self::Table), ResultCode> {
        let schema = "CREATE TABLE csv_data(name TEXT, age TEXT, city TEXT)".into();
        Ok((schema, CsvTable {}))
    }
}

struct CsvTable {}

impl VTable for CsvTable {
    type Cursor = CsvCursor;
    type Error = &'static str;

    fn open(&self, _conn: Option<std::rc::Rc<Connection>>) -> Result<Self::Cursor, Self::Error> {
        Ok(CsvCursor { rows: Vec::new(), index: 0 })
    }

    // Optional methods for writable tables:
    fn update(&mut self, _rowid: i64, _args: &[Value]) -> Result<(), Self::Error> { Ok(()) }
    fn insert(&mut self, _args: &[Value]) -> Result<i64, Self::Error> { Ok(0) }
    fn delete(&mut self, _rowid: i64) -> Result<(), Self::Error> { Ok(()) }
}

#[derive(Debug)]
struct CsvCursor {
    rows: Vec<Vec<String>>,
    index: usize,
}

impl VTabCursor for CsvCursor {
    type Error = &'static str;

    fn filter(&mut self, _args: &[Value], _idx_info: Option<(&str, i32)>) -> ResultCode {
        ResultCode::OK
    }

    fn next(&mut self) -> ResultCode {
        if self.index + 1 < self.rows.len() {
            self.index += 1;
            ResultCode::OK
        } else {
            ResultCode::EOF
        }
    }

    fn eof(&self) -> bool {
        self.index >= self.rows.len()
    }

    fn column(&self, idx: u32) -> Result<Value, Self::Error> {
        let row = &self.rows[self.index];
        Ok(row.get(idx as usize).map(|s| Value::from_text(s)).unwrap_or_else(Value::null))
    }

    fn rowid(&self) -> i64 {
        self.index as i64
    }
}
```

### Querying through the engine connection

A virtual table can be handed an `Rc<Connection>` to query the same underlying
connection that created it, using the engine's prepared-statement API:

```rust
let mut stmt = self.connection.prepare("SELECT col FROM table WHERE name = ?;");
stmt.bind_at(std::num::NonZeroUsize::new(1).unwrap(), args[0]);

while let StepResult::Row = stmt.step() {
    let row = stmt.get_row();
    if let Some(val) = row.first() {
        println!("result: {:?}", val);
    }
}
stmt.close();
```

## VFS example

Implement `VfsExtension` (and `VfsFile` for the file handle) to extend the
engine's OS interface. Requires the `vfs` feature.

```rust
use oxisqlite_ext::{ExtResult as Result, ResultCode, VfsDerive, VfsExtension, VfsFile};
use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};

#[derive(VfsDerive, Default)]
struct ExampleFS;

struct ExampleFile {
    file: std::fs::File,
}

impl VfsExtension for ExampleFS {
    const NAME: &'static str = "example";
    type File = ExampleFile;

    fn open(&self, path: &str, flags: i32, _direct: bool) -> Result<Self::File> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(flags & 1 != 0)
            .open(path)
            .map_err(|_| ResultCode::Error)?;
        Ok(ExampleFile { file })
    }
}

impl VfsFile for ExampleFile {
    fn read(&mut self, buf: &mut [u8], count: usize, offset: i64) -> Result<i32> {
        self.file.seek(SeekFrom::Start(offset as u64)).map_err(|_| ResultCode::Error)?;
        self.file.read(&mut buf[..count]).map_err(|_| ResultCode::Error).map(|n| n as i32)
    }

    fn write(&mut self, buf: &[u8], count: usize, offset: i64) -> Result<i32> {
        self.file.seek(SeekFrom::Start(offset as u64)).map_err(|_| ResultCode::Error)?;
        self.file.write(&buf[..count]).map_err(|_| ResultCode::Error).map(|n| n as i32)
    }

    fn sync(&self) -> Result<()> {
        self.file.sync_all().map_err(|_| ResultCode::Error)
    }

    fn size(&self) -> i64 {
        self.file.metadata().map(|m| m.len() as i64).unwrap_or(-1)
    }
}
```

## Fork lineage & licensing

Part of a COOLJAPAN C-free fork of limbo 0.0.22 (MIT). Notably, the upstream
extension instructions that required wiring in a C allocator for dynamically
linked extensions do **not** apply here: the fork removed that C allocator and
the global-allocator injection entirely, so building and registering an
extension pulls in no C. Full attribution and per-component licensing are
recorded in the repo-root [`/NOTICE`](../../NOTICE).

Copyright © 2024–2026 COOLJAPAN OU (Team Kitasan). COOLJAPAN code is licensed
under **Apache-2.0**; upstream limbo code remains under MIT (see
[`/NOTICE`](../../NOTICE)).

Part of the [OxiSQL](../../README.md) workspace.
