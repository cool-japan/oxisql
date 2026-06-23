//! Shared Criterion profiler that emits a `pprof` flamegraph per benchmark.
//!
//! This is implemented locally instead of using `pprof::criterion::PProfProfiler`
//! because `pprof`'s bundled Criterion integration is still pinned to
//! `criterion` 0.5, whereas this crate tracks the latest `criterion` (0.8).
//! Wrapping `pprof`'s raw `ProfilerGuard` against Criterion's own `Profiler`
//! trait keeps flamegraph profiling available without pulling a second,
//! conflicting `criterion` version into the dependency graph.

use std::fs::File;
use std::os::raw::c_int;
use std::path::Path;

use criterion::profiler::Profiler;
use pprof::ProfilerGuard;

/// A Criterion [`Profiler`] that records a sampling profile with `pprof` while a
/// benchmark is being profiled (via `--profile-time`) and writes a
/// `flamegraph.svg` into the benchmark's output directory.
pub struct FlamegraphProfiler {
    frequency: c_int,
    active_profiler: Option<ProfilerGuard<'static>>,
}

impl FlamegraphProfiler {
    /// Create a profiler that samples at `frequency` hertz.
    pub fn new(frequency: c_int) -> Self {
        Self {
            frequency,
            active_profiler: None,
        }
    }
}

impl Profiler for FlamegraphProfiler {
    fn start_profiling(&mut self, _benchmark_id: &str, _benchmark_dir: &Path) {
        let guard = ProfilerGuard::new(self.frequency).expect("failed to start pprof profiler");
        self.active_profiler = Some(guard);
    }

    fn stop_profiling(&mut self, _benchmark_id: &str, benchmark_dir: &Path) {
        let Some(profiler) = self.active_profiler.take() else {
            return;
        };
        std::fs::create_dir_all(benchmark_dir)
            .expect("failed to create benchmark output directory");
        let flamegraph_path = benchmark_dir.join("flamegraph.svg");
        let flamegraph_file =
            File::create(&flamegraph_path).expect("failed to create flamegraph.svg");
        profiler
            .report()
            .build()
            .expect("failed to build pprof report")
            .flamegraph(flamegraph_file)
            .expect("failed to write flamegraph");
    }
}
