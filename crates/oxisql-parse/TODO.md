# oxisql-parse — TODO

## Status

**Stable** · version 0.3.2 · **129 tests**.

A `sqlparser` facade with dialect-aware parsing, a fluent `QueryBuilder`, a
logical planner, a rule-based optimizer, DML planning, a cost model, a schema
validator, aggregate / window helpers, an `explain` pretty-printer, and an LRU
parse cache. Pure Rust. **No feature flags.**

## Done

### Parsing & analysis
- [x] `parse` / `parse_one` / `parse_with_dialect` / `parse_one_with_dialect`
- [x] Dialect shorthands: `parse_postgres`, `parse_mysql`, `parse_sqlite`; `SqlDialect` enum (`Generic` / `Postgres` / `MySQL` / `SQLite`)
- [x] `format(stmt)` — AST back to SQL text via sqlparser `Display`
- [x] `analysis` module: `is_read_only`, `normalize`, `extract_tables`, `extract_columns`, `count_params`
- [x] Re-export common sqlparser AST types (`Expr`, `SelectItem`, `TableFactor`, `JoinConstraint`, …)

### Query builder
- [x] Fluent `QueryBuilder` — `select` / `select_all` / `distinct` / `from` / `join` / `left_join` / `right_join` / `where_clause` / `group_by` / `having` / `order_by` / `limit` / `offset` / `build` / `build_and_parse` / `build_ref`
- [x] Static DML helpers: `insert`, `update`, `delete`

### Logical planning
- [x] `plan_query` / `plan_statement` → `LogicalPlan` (`Scan`, `Filter`, `Projection`, `Join`, `Aggregate`, `Sort`, `Limit`, `SetOp`, `Subquery`, `Values`, `Empty`)
- [x] `JoinType` (`Inner` / `Left` / `Right` / `Full` / `Cross`)
- [x] Aggregate planning — `extract_aggregates`, `AggFunc` (`Count`/`Sum`/`Avg`/`Min`/`Max`); HAVING → `Filter` over `Aggregate`
- [x] Window functions — `ROW_NUMBER`, `RANK`, `DENSE_RANK`, `LAG`, `LEAD`, `NTILE` with `OVER (PARTITION BY … ORDER BY …)`
- [x] Subqueries (scalar / EXISTS / IN / correlated) and CTEs (WITH, recursive)
- [x] Set operations — UNION / INTERSECT / EXCEPT

### Optimizer
- [x] `optimize(plan)` default pipeline + `Optimizer` builder
- [x] Passes: `PredicatePushdown`, `ProjectionPruning`, `ConstantFolding`, `LimitPushThrough`, `JoinAlgorithmPass` (all implement `OptPass`)
- [x] Join-algorithm selection hints (hash / merge / nested-loop)

### DML planning
- [x] `plan_dml` → `DmlPlan` (`Insert`, `InsertSelect`, `Update`, `Upsert`, `Delete`)
- [x] INSERT … SELECT, UPDATE … FROM (Postgres multi-table), UPSERT (ON CONFLICT / ON DUPLICATE KEY UPDATE)

### Cost, validation, explain, cache
- [x] Cost model — `CostModel`, `TableStats`, `CostEstimate`
- [x] Schema validation — `SchemaValidator`, `ValidationError` (`UnknownTable` / `UnknownColumn` / `AmbiguousColumn`)
- [x] `explain(plan, verbose)` — human-readable plan tree
- [x] `ParseCache` — thread-safe LRU keyed by `(sql, dialect)`; `new` / `parse` / `len` / `is_empty` / `clear`

### Testing & performance
- [x] Parse tests for SELECT / INSERT / UPDATE / DELETE / CREATE / DROP / ALTER, positional params, complex JOIN / subquery / CTE / window queries, and malformed-SQL error messages
- [x] Dialect-specific syntax tests (Postgres `::` cast, MySQL backticks, SQLite `AUTOINCREMENT`)
- [x] Planner-correctness and optimizer-pass tests
- [x] Benchmarks: parse throughput, plan overhead, optimizer-chain timing
- [x] Cross-crate integration: normalization in `oxisql-embedded`, logical-plan bridge in `oxisql-datafusion`, prepared-statement dedup via `normalize`

## Roadmap / next
- [x] Statistics-driven join reordering (use `CostModel` to pick join order, not just an algorithm hint) (planned 2026-06-10)
  - **Goal:** DPccp/GOO cost-based join reordering using per-column NDV statistics; optimal inner/cross join order without touching outer joins.
  - **Design:** Detect maximal contiguous Inner/Cross join nests. Atoms = leaf sub-plans costed via `CostModel::estimate`. Edges from equi-conjuncts `a.x=b.y` with selectivity `∏ 1/max(ndv)`. DPccp bottom-up DP over connected subsets (`best: HashMap<atom-bitset, (plan, cost)>`) for n≤12; GOO greedy fallback above. `JoinReorder { model: Arc<CostModel>, dp_threshold: usize }` as `OptPass`. Placed after `PredicatePushdown`, before `JoinAlgorithmPass`. `Optimizer::with_cost_model(CostModel)` registers the cost-aware pipeline (non-destructive to `Optimizer::new()`). `ColumnStats { ndv, null_fraction, min, max }` added to `TableStats`.
  - **Files:** `src/optimizer/join_reorder.rs` (new), `src/cost.rs` (+ColumnStats, +estimate_scan/estimate_join_from primitives), `src/optimizer/mod.rs` (+with_cost_model), `src/plan.rs` (+JoinType::LeftSemi/LeftAnti needed by item 3), `Cargo.toml` (+proptest dev-dep)
  - **Tests:** 3-table chain small-tables-first; outer-join nest untouched; cross-join-only; DP-vs-greedy parity; proptest cost-monotonicity + multiset-preserved + idempotent
  - **Risk:** DPccp exponential (n≤12 cap); GOO fallback prevents blowup. Outer-join correctness: strict stop at Left/Right/Full boundaries.
- [x] Common-subexpression elimination and predicate simplification passes (planned 2026-06-10)
  - **Goal:** Boolean/range predicate simplification to fixpoint; plan-level CSE for identical subquery bodies; intra-expression CSE via Compute binding node.
  - **Design:** `PredicateSimplification` pass: parse predicate→`Expr`, bounded-fixpoint rewrite (constant folding via `folding.rs fold_expr`; boolean algebra: `x AND TRUE→x`, idempotence via canonical_hash, NOT NOT, complement, comparison negation; per-column range coalescing `a>5 AND a>3→a>5`; contradiction `a>10 AND a<5→FALSE`; equality dominance; `a IN (1)→a=1`). `CommonSubexprElimination` pass: plan-level hoist of canonical-hash-identical Subquery/Exists/InSubquery bodies into Cte+CteRef; intra-expression via new `LogicalPlan::Compute { input, bindings }` node. `src/optimizer/expr_util.rs` (new shared module) provides parse_predicate/render, split_conjuncts/join_conjuncts, equi_key, collect_colrefs, canonical_hash.
  - **Files:** `src/optimizer/expr_util.rs` (new), `src/optimizer/simplify.rs` (new, PredicateSimplification), `src/optimizer/cse.rs` (new, CommonSubexprElimination), `src/plan.rs` (+Compute variant), `src/explain.rs` (+Compute arm), `src/cost.rs` (+Compute cost), `crates/oxisql-datafusion/src/plan_bridge.rs` (+Compute arm in plan_node_name)
  - **Tests:** `1=1 AND x>0→x>0`; `x AND FALSE→Empty`; `a>5 AND a>3→a>5`; contradiction→Empty; duplicate scalar subquery→1 Cte; proptest boolean-equivalence + idempotent
  - **Risk:** Compute node propagation through all passes; datafusion plan_bridge must compile. Escalation: if Compute destabilizes, fall back to plan-level-only CSE + intra-expr detection-only.
- [x] Decorrelation of correlated subqueries into joins in the planner (currently structural) (planned 2026-06-10)
  - **Goal:** Correlated EXISTS/NOT EXISTS → LeftSemi/LeftAnti joins; correlated IN → semi/anti; scalar correlated aggregate → LEFT JOIN on grouped Aggregate.
  - **Design:** Thread `outer_scope: &[String]` through `plan_select`. Split WHERE on top-level AND via `expr_util::split_conjuncts`. Per conjunct: correlated `EXISTS`→LeftSemi, `NOT EXISTS`→LeftAnti, `IN`→semi/anti with `on=(outer=inner_col) AND corr`, scalar correlated aggregate→LEFT JOIN against `Aggregate{group_by=[corr_key], …}`. Correlation test: any inner-conjunct qualifier ∈ outer_scope (via collect_colrefs). Uncorrelated subqueries stay structural. `PlannerOptions { decorrelate: bool }` + `plan_query_with(query, opts)` (default decorrelates). `JoinType::LeftSemi/LeftAnti` already added by item 1 prerequisite.
  - **Files:** `src/decorrelate.rs` (new), `src/planner.rs` (hook in plan_select WHERE handling), `src/plan.rs` (LeftSemi/LeftAnti — shared with item 1 foundation), `src/lib.rs` (re-export plan_query_with, PlannerOptions)
  - **Tests:** correlated EXISTS→SEMI; NOT EXISTS→ANTI; correlated IN→semi; scalar aggregate→LEFT JOIN; uncorrelated subquery unchanged; `plan_query_with(q, PlannerOptions{decorrelate:false})` preserves structural
  - **Risk:** Only decorrelates recognized shapes; unknown scalar → falls back to Subquery structural node (not dropped). Must not break existing uncorrelated tests.
- [x] Parameterized-plan cache (cache plans, not just parsed ASTs, keyed by normalized SQL) (planned 2026-06-10)
  - **Goal:** `PlanCache` keyed by literal-normalized SQL template; misses parse+optimize+cache; hits return `Arc<LogicalPlan>` instantly; schema invalidation via generation counter.
  - **Design:** `parameterize(sql) -> ParameterizedSql { template, literals }` — lexical literal→`?` pass (numbers/strings/booleans/NULL/negatives/floats/scientific; safe over quoted identifiers). Key = `{ normalize(parameterize(sql).template), dialect, generation: AtomicU64 }`. On miss: parse template (placeholders are valid SQL) → plan_statement → optimize → store `Arc<LogicalPlan>` in `LruCache<Key, Arc<LogicalPlan>>` behind `Mutex`. `invalidate_schema()` bumps generation; stale keys age out of LRU. Mirrors `ParseCache` poisoned-lock handling.
  - **Files:** `src/parameterize.rs` (new), `src/plan_cache.rs` (new), `src/lib.rs` (re-export), `Cargo.toml` (proptest dev-dep)
  - **Tests:** parameterize literals all kinds; two queries differing in literals → 1 cache entry, second is a hit; different dialect → distinct entries; invalidate_schema forces rebuild; thread-safety smoke test; proptest parameterize-is-fixpoint
  - **Risk:** Literal→? transform must be safe over quoted identifiers and escaped quotes; full lexical scanner rather than regex.
- [x] Richer `explain` output: per-node cost / cardinality estimates and an optional JSON format (planned 2026-06-10)
  - **Goal:** `explain_verbose` annotates each plan node with `rows=…, cost=…`; `explain_json` emits valid JSON with optional cost; existing `explain(plan)` unchanged.
  - **Design:** Out-of-band cost tree: `NodeCost { op, estimate: CostEstimate, children: Vec<NodeCost> }` + `CostModel::explain_costs(plan) -> NodeCost` (reuses estimate_scan/estimate_join_from primitives from item 1). `explain_verbose(plan, &CostModel) -> String`: each line gets ` (rows=N, cost=M)` suffix. `explain_json(plan, Option<&CostModel>) -> String`: hand-rolled ~80-line JSON writer (avoid pulling serde_json as a library dep; serde_json is dev-only for test validation). No plan field changes → no datafusion bridge impact.
  - **Files:** `src/cost.rs` (+NodeCost, +explain_costs), `src/explain.rs` (+explain_verbose, +explain_json), `src/lib.rs` (re-export), `Cargo.toml` (+serde_json dev-dep for JSON test validation)
  - **Tests:** explain_verbose lines contain `rows=`/`cost=`; 50k-row Scan shows rows=50000; explain_json valid JSON (parsed back via serde_json); JSON escaping of quotes/backslash/newline; filter rows < child rows; proptest explain_costs isomorphic to plan
  - **Risk:** JSON writer must handle all Value string representations; hand-rolled writer's escaping. Low risk since it's additive.

## Known limitations
None of consequence. Parsing fidelity tracks the pinned `sqlparser` version, so
brand-new vendor-specific syntax may require a `sqlparser` bump; some optimizer
passes are conservative (annotate/structural rather than cost-driven), which is
captured under Roadmap above rather than as a defect.
